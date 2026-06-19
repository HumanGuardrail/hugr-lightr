#[cfg(test)]
mod cases {
    use crate::{
        Digest, Entry, LightrError, Manifest, RefRecord, ref_key, validate_ref_name,
        REF_KEY_DOMAIN,
    };

    // Known BLAKE3 vectors
    #[test]
    fn digest_known_vector_empty() {
        // blake3 of empty bytes — known test vector
        let d = Digest::of_bytes(b"");
        let hex = d.to_hex();
        assert_eq!(
            hex,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn digest_known_vector_lightr() {
        // blake3 of b"lightr"
        let d = Digest::of_bytes(b"lightr");
        // Compute expected dynamically to avoid hard-coding a potentially wrong
        // vector — we verify it equals what blake3 returns.
        let expected = blake3::hash(b"lightr");
        assert_eq!(d.0, *expected.as_bytes());
        assert_eq!(d.to_hex().len(), 64);
    }

    #[test]
    fn digest_hex_roundtrip() {
        let original = Digest::of_bytes(b"roundtrip test");
        let hex = original.to_hex();
        assert_eq!(hex.len(), 64);
        // All lowercase
        assert!(hex.chars().all(|c| !c.is_uppercase()));
        let recovered = Digest::from_hex(&hex).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn digest_from_hex_uppercase() {
        // Accept uppercase hex
        let d = Digest::of_bytes(b"upper");
        let hex_lower = d.to_hex();
        let hex_upper = hex_lower.to_uppercase();
        let recovered = Digest::from_hex(&hex_upper).unwrap();
        assert_eq!(d, recovered);
    }

    #[test]
    fn digest_from_hex_invalid() {
        // Wrong length
        assert!(Digest::from_hex("abc").is_err());
        // Non-hex chars
        let bad: String = "g".repeat(64);
        assert!(Digest::from_hex(&bad).is_err());
    }

    #[test]
    fn digest_debug_is_lowercase_hex() {
        let d = Digest::of_bytes(b"debug");
        let dbg = format!("{d:?}");
        assert_eq!(dbg.len(), 64);
        assert!(dbg
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    // Manifest encode/decode roundtrip — all 3 entry kinds, path-sorted
    #[test]
    fn manifest_roundtrip_all_kinds() {
        let digest_a = Digest::of_bytes(b"file-a");
        let manifest = Manifest {
            version: 1,
            total_size: 1234,
            entries: vec![
                Entry::Dir {
                    path: "a/empty_dir".to_string(),
                },
                Entry::File {
                    path: "a/file.txt".to_string(),
                    mode: 0o644,
                    size: 42,
                    digest: digest_a,
                },
                Entry::Symlink {
                    path: "b/link".to_string(),
                    target: "../a/file.txt".to_string(),
                },
            ],
        };
        let encoded = manifest.encode();
        let decoded = Manifest::decode(&encoded).unwrap();
        assert_eq!(manifest, decoded);
    }

    #[test]
    fn manifest_decode_rejects_bad_magic() {
        let mut bytes = vec![0u8; 20];
        bytes[0] = b'X';
        bytes[1] = b'X';
        bytes[2] = b'X';
        bytes[3] = b'X';
        let err = Manifest::decode(&bytes).unwrap_err();
        assert!(matches!(err, LightrError::InvalidManifest(_)));
    }

    #[test]
    fn manifest_decode_rejects_truncation() {
        let manifest = Manifest {
            version: 1,
            total_size: 0,
            entries: vec![Entry::Dir {
                path: "x".to_string(),
            }],
        };
        let full = manifest.encode();
        // Truncate to partial
        let truncated = &full[..full.len() - 2];
        let err = Manifest::decode(truncated).unwrap_err();
        assert!(matches!(err, LightrError::InvalidManifest(_)));
    }

    #[test]
    fn manifest_digest_is_of_encode() {
        let manifest = Manifest {
            version: 1,
            total_size: 99,
            entries: vec![Entry::Dir {
                path: "root".to_string(),
            }],
        };
        let expected = Digest::of_bytes(&manifest.encode());
        assert_eq!(manifest.digest(), expected);
    }

    // RefRecord roundtrip — with and without parent
    #[test]
    fn refrecord_roundtrip_with_parent() {
        let root = Digest::of_bytes(b"root");
        let parent = Digest::of_bytes(b"parent");
        let rec = RefRecord {
            name: "web".to_string(),
            root,
            parent: Some(parent),
            created_at_unix: 1_700_000_000,
            tool_version: "0.1.0".to_string(),
        };
        let encoded = rec.encode();
        let decoded = RefRecord::decode(&encoded).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn refrecord_roundtrip_without_parent() {
        let root = Digest::of_bytes(b"root2");
        let rec = RefRecord {
            name: "@hugr/web".to_string(),
            root,
            parent: None,
            created_at_unix: 0,
            tool_version: "0.1.0-alpha".to_string(),
        };
        let encoded = rec.encode();
        let decoded = RefRecord::decode(&encoded).unwrap();
        assert_eq!(rec, decoded);
    }

    // validate_ref_name — accept/reject table
    #[test]
    fn ref_name_accept() {
        assert!(validate_ref_name("web").is_ok());
        assert!(validate_ref_name("@hugr/web").is_ok());
        assert!(validate_ref_name("a.b_c-d").is_ok());
    }

    #[test]
    fn ref_name_reject_empty() {
        assert!(validate_ref_name("").is_err());
    }

    #[test]
    fn ref_name_reject_uppercase_local() {
        assert!(validate_ref_name("Web").is_err());
    }

    #[test]
    fn ref_name_reject_uppercase_namespace() {
        assert!(validate_ref_name("@HUGR/x").is_err());
    }

    #[test]
    fn ref_name_reject_namespace_no_local() {
        // "@a/" — namespace with empty local part
        assert!(validate_ref_name("@a/").is_err());
    }

    #[test]
    fn ref_name_reject_slash_without_namespace() {
        // "a/b" — slash without leading @namespace
        assert!(validate_ref_name("a/b").is_err());
    }

    #[test]
    fn ref_name_reject_65_char_name() {
        let long = "a".repeat(65);
        assert!(validate_ref_name(&long).is_err());
    }

    #[test]
    fn ref_name_accept_64_char_name() {
        let exactly64 = "a".repeat(64);
        assert!(validate_ref_name(&exactly64).is_ok());
    }

    #[test]
    fn ref_name_reject_space_in_namespace() {
        assert!(validate_ref_name("@a b/c").is_err());
    }

    // ref_key domain separation
    #[test]
    fn ref_key_domain_separation() {
        let name = "web";
        let key = ref_key(name);
        let raw = Digest::of_bytes(name.as_bytes());
        // Must differ from plain hash of name
        assert_ne!(key, raw);
        // Must equal the two-update approach
        let mut hasher = blake3::Hasher::new();
        hasher.update(REF_KEY_DOMAIN.as_bytes());
        hasher.update(name.as_bytes());
        let expected = Digest(*hasher.finalize().as_bytes());
        assert_eq!(key, expected);
    }
}
