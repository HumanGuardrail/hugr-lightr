//! DHCP frame parsing: Ethernet → IPv4 → UDP → BOOTP/DHCP.

use super::DhcpRequest;
use super::{
    BOOTP_FIXED_LEN, BOOTP_FLAG_BROADCAST, BOOTREQUEST, DHCP_CLIENT_PORT, DHCP_MAGIC_COOKIE,
    DHCP_SERVER_PORT, ETHERTYPE_IPV4, ETH_HDR_LEN, HLEN_ETHERNET, HTYPE_ETHERNET, IPV4_MIN_HDR_LEN,
    IPV4_MIN_IHL_WORDS, IP_PROTO_UDP, OPT_END, OPT_MESSAGE_TYPE, OPT_PAD, UDP_HDR_LEN,
};

// ---------------------------------------------------------------------------
// BE helpers (module-local)
// ---------------------------------------------------------------------------

pub(super) fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

pub(super) fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse Ethernet → IPv4 → UDP(68→67) → BOOTP/DHCP. Returns `None` the moment
/// any layer fails to match (fail-closed: a non-DHCP frame is never touched).
pub(super) fn parse_dhcp(frame: &[u8]) -> Option<DhcpRequest> {
    // --- Ethernet ---
    if frame.len() < ETH_HDR_LEN {
        return None;
    }
    let ethertype = be16(&frame[12..14]);
    if ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    let l3 = &frame[ETH_HDR_LEN..];

    // --- IPv4 ---
    if l3.len() < IPV4_MIN_HDR_LEN {
        return None;
    }
    let version = l3[0] >> 4;
    let ihl_words = l3[0] & 0x0f;
    if version != 4 || ihl_words < IPV4_MIN_IHL_WORDS {
        return None;
    }
    let ip_hdr_len = ihl_words as usize * 4;
    if l3.len() < ip_hdr_len {
        return None;
    }
    if l3[9] != IP_PROTO_UDP {
        return None;
    }
    // total_length bounds the L4 payload (guards against trailing padding).
    let total_len = be16(&l3[2..4]) as usize;
    if total_len < ip_hdr_len || total_len > l3.len() {
        return None;
    }
    let l4 = &l3[ip_hdr_len..total_len];

    // --- UDP ---
    if l4.len() < UDP_HDR_LEN {
        return None;
    }
    let src_port = be16(&l4[0..2]);
    let dst_port = be16(&l4[2..4]);
    if src_port != DHCP_CLIENT_PORT || dst_port != DHCP_SERVER_PORT {
        return None;
    }
    let udp_len = be16(&l4[4..6]) as usize;
    if udp_len < UDP_HDR_LEN || udp_len > l4.len() {
        return None;
    }
    let bootp = &l4[UDP_HDR_LEN..udp_len];

    // --- BOOTP / DHCP ---
    parse_bootp(bootp)
}

/// Parse the BOOTP fixed header + DHCP options. Requires a BOOTREQUEST, an
/// Ethernet hardware type, the magic cookie, and an option-53 message type.
pub(super) fn parse_bootp(bootp: &[u8]) -> Option<DhcpRequest> {
    // Need the fixed header + magic cookie at minimum.
    if bootp.len() < BOOTP_FIXED_LEN + 4 {
        return None;
    }
    let op = bootp[0];
    let htype = bootp[1];
    let hlen = bootp[2];
    if op != BOOTREQUEST || htype != HTYPE_ETHERNET || hlen != HLEN_ETHERNET {
        return None;
    }

    let mut xid = [0u8; 4];
    xid.copy_from_slice(&bootp[4..8]);
    let mut secs = [0u8; 2];
    secs.copy_from_slice(&bootp[8..10]);
    let flags = be16(&bootp[10..12]);
    let broadcast = flags & BOOTP_FLAG_BROADCAST != 0;

    let mut giaddr = [0u8; 4];
    giaddr.copy_from_slice(&bootp[24..28]);

    // chaddr is 16 bytes at offset 28; the first `hlen` (6) are the MAC.
    let mut chaddr = [0u8; 6];
    chaddr.copy_from_slice(&bootp[28..34]);

    // Magic cookie precedes the options area.
    if be32(&bootp[BOOTP_FIXED_LEN..BOOTP_FIXED_LEN + 4]) != DHCP_MAGIC_COOKIE {
        return None;
    }
    let options = &bootp[BOOTP_FIXED_LEN + 4..];
    let msg_type = parse_option_53(options)?;

    Some(DhcpRequest {
        chaddr,
        xid,
        secs,
        broadcast,
        giaddr,
        msg_type,
    })
}

/// Walk the TLV options looking for option 53 (DHCP message type). Handles PAD
/// (0, no length) and stops at END (255). Returns `None` if absent/malformed.
pub(super) fn parse_option_53(options: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i < options.len() {
        let code = options[i];
        if code == OPT_END {
            break;
        }
        if code == OPT_PAD {
            i += 1;
            continue;
        }
        // code + len + value
        if i + 1 >= options.len() {
            return None;
        }
        let len = options[i + 1] as usize;
        let val_start = i + 2;
        let val_end = val_start.checked_add(len)?;
        if val_end > options.len() {
            return None;
        }
        if code == OPT_MESSAGE_TYPE {
            if len != 1 {
                return None;
            }
            return Some(options[val_start]);
        }
        i = val_end;
    }
    None
}
