//! Seam conversions: CANONICAL vocab (`cri_canon`, sibling lightr-cri) ⇄ LOCAL
//! vocab (`lightr_cri_backend`, this workspace).
//!
//! The two vocab crates are PARALLEL TRANSCRIPTIONS of the frozen seam —
//! structurally identical, nominally distinct (same crate name, different repo).
//! So every conversion below is a mechanical field-by-field move. Helpers are
//! named by direction:
//!   * `c2l_*` = canonical → local  (incoming method ARGS)
//!   * `l2c_*` = local → canonical  (outgoing RESULTS / errors)
//!
//! KNOWN DRIFT (reported to the lead, not papered over): the LOCAL
//! `ContainerConfig` carries a v1.2 `security: Option<SecurityContext>` field
//! (owner-approved 2026-06-25, for KPI-4 AppArmor) that the CANONICAL seam
//! (still v1.1) does NOT have. Conversions therefore:
//!   * `c2l_container_cfg` sets `security: None` — a kubelet AppArmor profile
//!     CANNOT reach the backend through this composed path until the canonical
//!     seam is bumped to v1.2.
//!   * `l2c_container_cfg` DROPS `security` — it has no canonical counterpart.
//!
//! Both are loss-free in the wire sense (the canonical side never had the field)
//! but mean the v1.2 security extension is currently UNREACHABLE end-to-end.

use cri_canon as canon;
use lightr_cri_backend as local;

// ── Errors / Result ──────────────────────────────────────────────────────────

pub fn l2c_err(e: local::BackendError) -> canon::BackendError {
    use local::BackendError as L;
    match e {
        L::NotFound(m) => canon::BackendError::NotFound(m),
        L::AlreadyExists(m) => canon::BackendError::AlreadyExists(m),
        L::InvalidArgument(m) => canon::BackendError::InvalidArgument(m),
        L::FailedPrecondition(m) => canon::BackendError::FailedPrecondition(m),
        L::InUse(m) => canon::BackendError::InUse(m),
        L::Internal(m) => canon::BackendError::Internal(m),
        L::Io(e) => canon::BackendError::Io(e), // std::io::Error moves as-is
    }
}

// ── Identifiers ──────────────────────────────────────────────────────────────

pub fn c2l_sandbox_id(x: &canon::SandboxId) -> local::SandboxId {
    local::SandboxId(x.0.clone())
}
pub fn l2c_sandbox_id(x: local::SandboxId) -> canon::SandboxId {
    canon::SandboxId(x.0)
}
pub fn c2l_container_id(x: &canon::ContainerId) -> local::ContainerId {
    local::ContainerId(x.0.clone())
}
pub fn l2c_container_id(x: local::ContainerId) -> canon::ContainerId {
    canon::ContainerId(x.0)
}

// ── Enums ────────────────────────────────────────────────────────────────────

pub fn c2l_protocol(p: canon::Protocol) -> local::Protocol {
    match p {
        canon::Protocol::Tcp => local::Protocol::Tcp,
        canon::Protocol::Udp => local::Protocol::Udp,
        canon::Protocol::Sctp => local::Protocol::Sctp,
    }
}
pub fn l2c_protocol(p: local::Protocol) -> canon::Protocol {
    match p {
        local::Protocol::Tcp => canon::Protocol::Tcp,
        local::Protocol::Udp => canon::Protocol::Udp,
        local::Protocol::Sctp => canon::Protocol::Sctp,
    }
}

pub fn c2l_sandbox_state(s: canon::SandboxState) -> local::SandboxState {
    match s {
        canon::SandboxState::Ready => local::SandboxState::Ready,
        canon::SandboxState::NotReady => local::SandboxState::NotReady,
    }
}
pub fn l2c_sandbox_state(s: local::SandboxState) -> canon::SandboxState {
    match s {
        local::SandboxState::Ready => canon::SandboxState::Ready,
        local::SandboxState::NotReady => canon::SandboxState::NotReady,
    }
}

pub fn c2l_container_state(s: canon::ContainerState) -> local::ContainerState {
    match s {
        canon::ContainerState::Created => local::ContainerState::Created,
        canon::ContainerState::Running => local::ContainerState::Running,
        canon::ContainerState::Exited => local::ContainerState::Exited,
        canon::ContainerState::Unknown => local::ContainerState::Unknown,
    }
}
pub fn l2c_container_state(s: local::ContainerState) -> canon::ContainerState {
    match s {
        local::ContainerState::Created => canon::ContainerState::Created,
        local::ContainerState::Running => canon::ContainerState::Running,
        local::ContainerState::Exited => canon::ContainerState::Exited,
        local::ContainerState::Unknown => canon::ContainerState::Unknown,
    }
}

// ── Leaf structs ─────────────────────────────────────────────────────────────

pub fn c2l_dns(d: canon::DnsConfig) -> local::DnsConfig {
    local::DnsConfig {
        servers: d.servers,
        searches: d.searches,
        options: d.options,
    }
}
pub fn l2c_dns(d: local::DnsConfig) -> canon::DnsConfig {
    canon::DnsConfig {
        servers: d.servers,
        searches: d.searches,
        options: d.options,
    }
}

pub fn c2l_port(p: canon::PortMapping) -> local::PortMapping {
    local::PortMapping {
        protocol: c2l_protocol(p.protocol),
        container_port: p.container_port,
        host_port: p.host_port,
        host_ip: p.host_ip,
    }
}
pub fn l2c_port(p: local::PortMapping) -> canon::PortMapping {
    canon::PortMapping {
        protocol: l2c_protocol(p.protocol),
        container_port: p.container_port,
        host_port: p.host_port,
        host_ip: p.host_ip,
    }
}

pub fn c2l_mount(m: canon::Mount) -> local::Mount {
    local::Mount {
        container_path: m.container_path,
        host_path: m.host_path,
        readonly: m.readonly,
    }
}
pub fn l2c_mount(m: local::Mount) -> canon::Mount {
    canon::Mount {
        container_path: m.container_path,
        host_path: m.host_path,
        readonly: m.readonly,
    }
}

pub fn c2l_auth(a: &canon::AuthConfig) -> local::AuthConfig {
    local::AuthConfig {
        username: a.username.clone(),
        password: a.password.clone(),
        auth: a.auth.clone(),
        server_address: a.server_address.clone(),
    }
}

// ── Configs ──────────────────────────────────────────────────────────────────

pub fn c2l_sandbox_cfg(c: canon::SandboxConfig) -> local::SandboxConfig {
    local::SandboxConfig {
        name: c.name,
        uid: c.uid,
        namespace: c.namespace,
        attempt: c.attempt,
        labels: c.labels,
        annotations: c.annotations,
        log_directory: c.log_directory,
        hostname: c.hostname,
        host_network: c.host_network,
        dns: c.dns.map(c2l_dns),
        port_mappings: c.port_mappings.into_iter().map(c2l_port).collect(),
    }
}
pub fn l2c_sandbox_cfg(c: local::SandboxConfig) -> canon::SandboxConfig {
    canon::SandboxConfig {
        name: c.name,
        uid: c.uid,
        namespace: c.namespace,
        attempt: c.attempt,
        labels: c.labels,
        annotations: c.annotations,
        log_directory: c.log_directory,
        hostname: c.hostname,
        host_network: c.host_network,
        dns: c.dns.map(l2c_dns),
        port_mappings: c.port_mappings.into_iter().map(l2c_port).collect(),
    }
}

pub fn c2l_container_cfg(c: canon::ContainerConfig) -> local::ContainerConfig {
    local::ContainerConfig {
        name: c.name,
        attempt: c.attempt,
        image_ref: c.image_ref,
        command: c.command,
        args: c.args,
        working_dir: c.working_dir,
        envs: c.envs,
        mounts: c.mounts.into_iter().map(c2l_mount).collect(),
        labels: c.labels,
        annotations: c.annotations,
        log_path: c.log_path,
        tty: c.tty,
        stdin: c.stdin,
        // DRIFT: canonical seam (v1.1) has no security field. A kubelet
        // AppArmor/seccomp/caps context cannot arrive through this path until the
        // canonical seam is bumped to v1.2. See module-level note.
        security: None,
    }
}
pub fn l2c_container_cfg(c: local::ContainerConfig) -> canon::ContainerConfig {
    canon::ContainerConfig {
        name: c.name,
        attempt: c.attempt,
        image_ref: c.image_ref,
        command: c.command,
        args: c.args,
        working_dir: c.working_dir,
        envs: c.envs,
        mounts: c.mounts.into_iter().map(l2c_mount).collect(),
        labels: c.labels,
        annotations: c.annotations,
        log_path: c.log_path,
        tty: c.tty,
        stdin: c.stdin,
        // DRIFT: local `security` (v1.2) has no canonical counterpart → dropped.
    }
}

// ── Status / records (results only → l2c) ────────────────────────────────────

pub fn l2c_sandbox_status(s: local::SandboxStatus) -> canon::SandboxStatus {
    canon::SandboxStatus {
        id: l2c_sandbox_id(s.id),
        config: l2c_sandbox_cfg(s.config),
        state: l2c_sandbox_state(s.state),
        created_at_nanos: s.created_at_nanos,
        ip: s.ip,
        netns_path: s.netns_path,
    }
}

pub fn l2c_container_status(s: local::ContainerStatus) -> canon::ContainerStatus {
    canon::ContainerStatus {
        id: l2c_container_id(s.id),
        sandbox: l2c_sandbox_id(s.sandbox),
        config: l2c_container_cfg(s.config),
        state: l2c_container_state(s.state),
        created_at_nanos: s.created_at_nanos,
        started_at_nanos: s.started_at_nanos,
        finished_at_nanos: s.finished_at_nanos,
        exit_code: s.exit_code,
        reason: s.reason,
        message: s.message,
    }
}

pub fn l2c_exec_result(r: local::ExecResult) -> canon::ExecResult {
    canon::ExecResult {
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
    }
}

pub fn l2c_stats(r: local::ContainerStatsRec) -> canon::ContainerStatsRec {
    canon::ContainerStatsRec {
        id: l2c_container_id(r.id),
        timestamp_nanos: r.timestamp_nanos,
        cpu_usage_core_nanos: r.cpu_usage_core_nanos,
        memory_working_set_bytes: r.memory_working_set_bytes,
    }
}

pub fn l2c_pulled(p: local::PulledImage) -> canon::PulledImage {
    canon::PulledImage {
        ref_name: p.ref_name,
        root_hex: p.root_hex,
        total_size: p.total_size,
    }
}

pub fn l2c_image_record(r: local::ImageRecord) -> canon::ImageRecord {
    canon::ImageRecord {
        id: r.id,
        ref_name: r.ref_name,
        size: r.size,
    }
}

pub fn l2c_fs_info(f: local::FsInfo) -> canon::FsInfo {
    canon::FsInfo {
        timestamp_nanos: f.timestamp_nanos,
        mountpoint: f.mountpoint,
        used_bytes: f.used_bytes,
        inodes_used: f.inodes_used,
    }
}

// ── Filters (args only → c2l) ────────────────────────────────────────────────

pub fn c2l_sandbox_filter(f: &canon::SandboxFilter) -> local::SandboxFilter {
    local::SandboxFilter {
        id: f.id.as_ref().map(c2l_sandbox_id),
        state: f.state.map(c2l_sandbox_state),
        label_selector: f.label_selector.clone(),
    }
}

pub fn c2l_container_filter(f: &canon::ContainerFilter) -> local::ContainerFilter {
    local::ContainerFilter {
        id: f.id.as_ref().map(c2l_container_id),
        sandbox: f.sandbox.as_ref().map(c2l_sandbox_id),
        state: f.state.map(c2l_container_state),
        label_selector: f.label_selector.clone(),
    }
}

// ── Streaming session (result only → l2c) ────────────────────────────────────

/// Bridges a LOCAL `ExitWaiter` so it satisfies the CANONICAL `ExitWaiter`
/// trait. The two waiter traits are nominally distinct (parallel transcriptions)
/// but identical in shape: a single `wait(self: Box<Self>) -> Result<i32>`.
struct WaiterBridge(Box<dyn local::ExitWaiter>);

impl canon::ExitWaiter for WaiterBridge {
    fn wait(self: Box<Self>) -> canon::Result<i32> {
        self.0.wait().map_err(l2c_err)
    }
}

/// Convert a local `StreamSession` into the canonical one. The fd fields are
/// plain `std::fs::File` (the SAME std type in both crates) so they move across
/// directly; only the boxed waiter needs the trait bridge above.
pub fn l2c_stream(s: local::StreamSession) -> canon::StreamSession {
    canon::StreamSession {
        stdin: s.stdin,
        stdout: s.stdout,
        stderr: s.stderr,
        pty_master: s.pty_master,
        waiter: Box::new(WaiterBridge(s.waiter)),
    }
}
