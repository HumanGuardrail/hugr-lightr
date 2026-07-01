//! Container start setup — NS-path planning + rootfs hydrate (linux only).
//!
//! Extracted from `container.rs` (behavior-preserving split): the ns-engine
//! `RunDescriptor` builder and the CAS→rootfs materializer. Both are `linux`
//! only; the `start_container` core flow in `container.rs` calls `build_ns_plan`.

#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
use crate::container_wait::synth_resolv_conf;
#[cfg(target_os = "linux")]
use crate::util::ContainerRecord;
#[cfg(target_os = "linux")]
use crate::vocab::{BackendError, ContainerId, Result};
#[cfg(target_os = "linux")]
use crate::LightrBackend;

#[cfg(target_os = "linux")]
impl LightrBackend {
    // ── WP-#99: NS-path planning + rootfs hydrate (linux only) ────────────────

    /// Build the `ns`-engine `RunDescriptor` (real image rootfs + pod netns) for an
    /// **isolation-expecting** pod — the caller has already confirmed the sandbox
    /// has a pinned netns. Returns `Err` (FAILING the container start) when the ns
    /// engine is unavailable or the image cannot hydrate.
    ///
    /// AUDIT FIX (#99): the previous `Option` contract silently fell back to an
    /// unisolated HOST process when hydrate/engine failed — for a pod that has an
    /// isolated netns, that is FALSE ISOLATION the kubelet cannot detect (the
    /// container is still reported `Running`). Fail-closed instead. host_network /
    /// no-CNI pods (no pinned netns) legitimately use the host path; the caller
    /// gates on that and never calls this.
    #[cfg(target_os = "linux")]
    pub(crate) fn build_ns_plan(
        &self,
        rec: &ContainerRecord,
        argv: &[String],
    ) -> Result<crate::ns_run::RunDescriptor> {
        // Read the sandbox record ONCE (netns path + the v1.1 dns/hostname config
        // for GAP 2/3). Clone the bits we need out so the cache lock is released
        // before the (longer) hydrate below.
        let (netns_path, sandbox_dns, sandbox_hostname) = {
            let cache = self.cache();
            let s = cache.sandboxes.get(&rec.sandbox.0).ok_or_else(|| {
                BackendError::Internal("build_ns_plan called without a pod sandbox".to_string())
            })?;
            (
                s.netns_path.clone(),
                s.config.dns.clone(),
                s.config.hostname.clone(),
            )
        };
        let netns_path = netns_path.ok_or_else(|| {
            BackendError::Internal("build_ns_plan called without a pod netns".to_string())
        })?;

        // The ns engine must be available (root + Linux). For an isolation-expecting
        // pod this is REQUIRED — an unavailable engine is a hard error, not a silent
        // host downgrade.
        lightr_engine::engine_for(lightr_engine::EngineKind::Ns).map_err(|e| {
            BackendError::Internal(format!(
                "ns engine unavailable for an isolation-expecting pod (container {}): {e}",
                rec.id.0
            ))
        })?;

        // Materialize the image rootfs from the CAS; a miss is a hard error (cannot
        // run the real container ⇒ refuse rather than run an unisolated host process).
        let rootfs = self
            .hydrate_rootfs(&rec.id, &rec.config.image_ref)
            .map_err(|e| {
                BackendError::Internal(format!(
                    "hydrate rootfs for container {} (image {:?}) failed: {e}",
                    rec.id.0, rec.config.image_ref
                ))
            })?;

        // Capabilities from the v1.2 security context, when present (CRI style).
        let (cap_add, cap_drop) = match rec
            .config
            .security
            .as_ref()
            .and_then(|s| s.capabilities.as_ref())
        {
            Some(c) => (c.add.clone(), c.drop.clone()),
            None => (Vec::new(), Vec::new()),
        };

        // WP-#106 (KPI 4): map the v1.2 security context's AppArmor profile to the
        // profile NAME the ns engine execs under (aa_change_onexec). READY-BUT-INERT
        // today: `rec.config.security` is usually `None` (the cross-repo seam #89 that
        // maps the kubelet's proto profile into this field is not landed), so this is
        // `None` and the start path is byte-identical to before. The mapping:
        //   Localhost      ⇒ the loaded profile name (`localhost_ref`)
        //   Unconfined     ⇒ "unconfined" (explicitly run unconfined)
        //   RuntimeDefault ⇒ None (inherit for now — a named runtime-default profile
        //                    is a future choice; documented, not yet wired)
        let apparmor: Option<String> = rec
            .config
            .security
            .as_ref()
            .and_then(|s| s.apparmor.as_ref())
            .and_then(|p| match p.profile_type {
                crate::vocab::ProfileType::Localhost => Some(p.localhost_ref.clone()),
                crate::vocab::ProfileType::Unconfined => Some("unconfined".to_string()),
                crate::vocab::ProfileType::RuntimeDefault => None,
            });

        // WP-#108 (seccomp): mirror the apparmor mapping above — `rec.config.security`
        // is usually `None` today (the cross-repo seam mapping the kubelet's proto
        // seccomp profile into this field is not landed), so this is `None` and the
        // start path is byte-identical to before. The mapping:
        //   Localhost      ⇒ the profile PATH (`localhost_ref`)
        //   Unconfined     ⇒ "unconfined" (explicitly run without a filter)
        //   RuntimeDefault ⇒ None (inherit for now — a named runtime-default profile
        //                    is a future choice; documented, not yet wired)
        let seccomp: Option<String> = rec
            .config
            .security
            .as_ref()
            .and_then(|s| s.seccomp.as_ref())
            .and_then(|p| match p.profile_type {
                crate::vocab::ProfileType::Localhost => Some(p.localhost_ref.clone()),
                crate::vocab::ProfileType::Unconfined => Some("unconfined".to_string()),
                crate::vocab::ProfileType::RuntimeDefault => None,
            });

        // WP-#107 (CRI GAP 1, "starting container with volume" + symlink-host-path):
        // map the CRI `ContainerConfig.mounts` to the descriptor. Resolve `host_path`
        // HOST-SIDE here (the symlink-host-path spec creates a symlink to the real
        // dir; the host path is a host concern, so the engine stays a pure
        // bind-mounter) — `canonicalize` follows symlinks AND yields an absolute path.
        // Fail-closed: a host_path that cannot be resolved (a missing volume) FAILS
        // the start rather than binding a wrong/absent source.
        let mut mounts: Vec<crate::ns_run::NsBindMount> =
            Vec::with_capacity(rec.config.mounts.len());
        for m in &rec.config.mounts {
            let resolved = std::fs::canonicalize(&m.host_path).map_err(|e| {
                BackendError::Internal(format!(
                    "resolve volume host_path {:?} for container {} failed: {e}",
                    m.host_path, rec.id.0
                ))
            })?;
            mounts.push(crate::ns_run::NsBindMount {
                host_path: resolved.to_string_lossy().into_owned(),
                container_path: m.container_path.clone(),
                readonly: m.readonly,
            });
        }

        // WP-#107 (CRI GAP 2, "DNS config"): synthesize the /etc/resolv.conf content
        // from the sandbox `DnsConfig`. `None`/all-empty ⇒ `None` (leave the image's
        // resolv.conf untouched). Standard resolv.conf format (nameserver/search/
        // options lines), what Docker/runc write.
        let resolv_conf = sandbox_dns.as_ref().and_then(synth_resolv_conf);

        // WP-#107 (CRI GAP 3, "set hostname"): the sandbox hostname. Empty ⇒ `None`
        // (no UTS ns / no sethostname — unchanged behavior).
        let hostname = if sandbox_hostname.is_empty() {
            None
        } else {
            Some(sandbox_hostname)
        };

        Ok(crate::ns_run::RunDescriptor {
            rootfs,
            argv: argv.to_vec(),
            cwd: rec.config.working_dir.clone(),
            env: rec.config.envs.clone(),
            netns_path: Some(netns_path),
            // Deterministic, flat leaf so `stop` can rebuild the path and
            // `cgroup.kill` it (the record also persists this name).
            cgroup_name: format!("lightr-cri-{}", rec.id.0),
            // The frozen seam carries no read-only / shm-size / init for a
            // container; defaults (the ns engine still gives a default 64 MiB
            // /dev/shm). read_only/shm/init become reachable when the seam grows them.
            read_only: false,
            shm_size: None,
            init: false,
            cap_add,
            cap_drop,
            // WP-#102: the exec-readiness pipe write end is created+injected by
            // `start_container_impl` right before spawn (so the fd's lifetime is the
            // spawn's). The plan itself carries None.
            exec_ready_fd: None,
            // WP-#106: ready-but-inert AppArmor profile (None until the seam #89 maps
            // the kubelet profile into rec.config.security). The ns engine applies it
            // via aa_change_onexec right before the container's execv (fail-closed).
            apparmor,
            // WP-#108: ready-but-inert seccomp profile (None until the seam maps the
            // kubelet profile into rec.config.security). The ns engine compiles it
            // before pivot and installs the cBPF filter right before execv (fail-closed).
            seccomp,
            // WP-#107 (CRI GAP 1/2/3): the volume bind mounts (host-side realpath'd),
            // the synthesized /etc/resolv.conf, and the sandbox hostname. The ns engine
            // applies them in PID 1 (mounts + resolv.conf + hostname/UTS), fail-closed.
            mounts,
            resolv_conf,
            hostname,
        })
    }

    /// Materialize the image rootfs for `cid` from the CAS store into a persistent
    /// per-container dir (`<home>/cri/containers/<cid>/rootfs`) via
    /// `lightr_index::hydrate`. The store name is the SAME `sanitize_ref` the image
    /// pull tagged the bytes under. Idempotent: a non-empty existing rootfs (a
    /// restart) is reused. Honest `Err` (mapped) when the ref is absent from the
    /// store or hydration fails — the caller treats that as a host-path fallback.
    #[cfg(target_os = "linux")]
    fn hydrate_rootfs(&self, cid: &ContainerId, image_ref: &str) -> Result<std::path::PathBuf> {
        let store = lightr_store::Store::open(self.home().join("store"))
            .map_err(crate::util::map_lightr_err)?;
        let store_name = crate::images::sanitize_ref(image_ref);
        let rootfs = self.containers_dir().join(&cid.0).join("rootfs");

        // Reuse an already-hydrated rootfs (restart of the same container).
        if rootfs.exists() {
            let nonempty = fs::read_dir(&rootfs)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            if nonempty {
                return Ok(rootfs);
            }
        }
        fs::create_dir_all(&rootfs).map_err(BackendError::Io)?;
        lightr_index::hydrate(&rootfs, &store, &store_name).map_err(crate::util::map_lightr_err)?;
        Ok(rootfs)
    }
}
