//! On-disk serde mirror for `spec.json` — split out of `run/types.rs` to keep
//! each file under the 400-line godfile cap (house convention, via
//! `#[path] mod specdisk;` in `types.rs`). Holds `SpecOnDisk` and the proto/
//! kind-tagged on-disk mount/port shapes. The RUNTIME types (`RunSpec` etc.)
//! stay in `types.rs`. Nothing here is keyed; this is the persisted shape.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(crate) struct MountOnDisk {
    pub ref_name: String,
    pub target: String,
}

// R-MOUNT / R-SPECDISK (parity-contract.md §0): the go-forward, proto/kind-tagged
// on-disk mount shape. Mirrors `run::mount::MountKind`. The legacy `MountOnDisk`
// above stays for read back-compat; `MountOnDisk2` is what new spec.json writes.
// `#[serde(tag = "kind")]` makes it a tagged enum on disk. PARSING/RESOLUTION
// behaviour is WP-VOL-1's job — this only freezes the serialized SHAPE.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum MountOnDisk2 {
    CasRef {
        ref_name: String,
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    HostBind {
        source: String,
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    NamedVolume {
        source: String,
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    AnonVolume {
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    Tmpfs {
        target: String,
        #[serde(default)]
        opts: Vec<String>,
    },
}

// R-SPECDISK (parity-contract.md §0): proto-tagged published-port shape. The
// legacy `ports: Vec<(u16,u16)>` stays for read back-compat (TCP-only); `ports2`
// carries the protocol so UDP can land without a second format bump. Behaviour
// (binding UDP) is a Networking-axis WP's job.
#[derive(Serialize, Deserialize)]
pub(crate) struct PortOnDisk {
    pub host: u16,
    pub container: u16,
    /// `"tcp"` (default) or `"udp"`.
    #[serde(default = "default_proto")]
    pub proto: String,
}

/// Serde default for [`PortOnDisk::proto`] — TCP, matching the legacy
/// `ports: Vec<(u16,u16)>` channel which was TCP-only.
pub fn default_proto() -> String {
    "tcp".to_string()
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SpecOnDisk {
    pub cwd: String,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
    pub mounts: Vec<MountOnDisk>,
    pub detached: bool,
    pub created_at_unix: u64,
    // Networking Phase 1: published (host, container) TCP ports the supervisor
    // forwards. `#[serde(default)]` keeps JSON back-compat: spec.json files
    // written before this field existed (no `ports`) still parse to an empty
    // Vec, so an old detached run never breaks on read.
    #[serde(default)]
    pub ports: Vec<(u16, u16)>,
    // WP-NET2: the engine that runs this detached job. `#[serde(default)]` →
    // "native" for spec.json files written before this field existed, so an old
    // detached run keeps the native supervisor branch. The vz branch (a Linux
    // container in a microVM, with host→guest port forwarding) is selected by
    // engine == "vz" AND a present `rootfs_ref`.
    #[serde(default = "default_engine")]
    pub engine: String,
    // WP-NET2: the rootfs ref the vz branch hydrates + boots. None for native
    // runs (serde default). Present ⇒ a vz container run.
    #[serde(default)]
    pub rootfs_ref: Option<String>,
    /// WP-DISC: explicit env vars set on the detached child (compose service
    /// discovery: <PEER>_HOST/<PEER>_PORT). serde-defaulted = back-compat. NOT a
    /// memo-key input (runtime addressing, like ports) — and detached runs aren't
    /// memoized anyway.
    #[serde(default)]
    pub env: Vec<(String, String)>,

    // ── R-SPECDISK (parity-contract.md §0) — additive Docker-parity fields. ──
    // ALL `#[serde(default)]` for back-compat with spec.json written before the
    // freeze-gate. The existing `env`/`mounts`/`ports` above are UNTOUCHED. The
    // population + behaviour of every field below is a Wave-A/B WP's job; the
    // freeze-gate only lands the SHAPE.
    //
    // LEAD ARBITRATION (env-split): `env` above stays the UNKEYED discovery
    // channel; `env_explicit` below (user `-e`/`--env-file`) is the ONLY env
    // that enters the memo key (R-KEY). Two distinct channels — never merged.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub rm: bool,
    #[serde(default)]
    pub restart: Option<String>,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    /// User `-e`/`--env-file` env — the ONLY env in the memo key (R-KEY).
    #[serde(default)]
    pub env_explicit: Vec<(String, String)>,
    #[serde(default)]
    pub stop_signal: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub network_alias: Vec<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub add_host: Vec<(String, String)>,
    #[serde(default)]
    pub dns: Vec<String>,
    /// Go-forward tagged mount shape (R-MOUNT). Legacy `mounts` above stays for
    /// read back-compat.
    #[serde(default)]
    pub mounts2: Vec<MountOnDisk2>,
    /// Go-forward proto-tagged port shape. Legacy `ports` above stays for read
    /// back-compat (TCP-only).
    #[serde(default)]
    pub ports2: Vec<PortOnDisk>,

    // ── RC-SEAM-FREEZE — additive RC carry-fields mirroring the new `RunSpec`
    // slots. ALL `#[serde(default)]` (back-compat: a pre-freeze spec.json parses
    // to the no-op default). `hostname`/`labels` already existed above; the rest
    // are new. RUNTIME-ONLY (never keyed); apply is a future RC WP's job.
    #[serde(default)]
    pub cap_add: Vec<String>,
    #[serde(default)]
    pub cap_drop: Vec<String>,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub init: bool,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub oom_score_adj: Option<i32>,
    #[serde(default)]
    pub pids_limit: Option<i64>,
    #[serde(default)]
    pub shm_size: Option<u64>,
}

/// Serde default for [`SpecOnDisk::engine`] — the native supervisor branch, so a
/// pre-WP-NET2 spec.json (no `engine` field) keeps its original behaviour.
pub fn default_engine() -> String {
    "native".to_string()
}

// R-SPECDISK (parity-contract.md §0): a manual `Default` whose field values
// MATCH the serde defaults exactly (notably `engine = "native"`, NOT the empty
// string a derive would give). This lets every existing `SpecOnDisk { … }`
// construction site append `..Default::default()` for the additive freeze-gate
// fields without touching any field it already sets — zero behaviour change.
impl Default for SpecOnDisk {
    fn default() -> Self {
        SpecOnDisk {
            cwd: String::new(),
            command: Vec::new(),
            env_keys: Vec::new(),
            mounts: Vec::new(),
            detached: false,
            created_at_unix: 0,
            ports: Vec::new(),
            engine: default_engine(),
            rootfs_ref: None,
            env: Vec::new(),
            name: None,
            rm: false,
            restart: None,
            labels: Vec::new(),
            workdir: None,
            user: None,
            entrypoint: None,
            env_explicit: Vec::new(),
            stop_signal: None,
            network: None,
            network_alias: Vec::new(),
            hostname: None,
            add_host: Vec::new(),
            dns: Vec::new(),
            mounts2: Vec::new(),
            ports2: Vec::new(),
            // RC-SEAM-FREEZE: no-op defaults (match serde defaults exactly).
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            tty: false,
            init: false,
            read_only: false,
            oom_score_adj: None,
            pids_limit: None,
            shm_size: None,
        }
    }
}
