//! The integration adapter: the real `LightrBackend` (this workspace) presented
//! as a CANONICAL `cri_canon::CriBackend` so the sibling `lightr-cri-server`
//! composition root can drive it unchanged.
//!
//! Each method converts canonical-typed ARGS → local types, calls the inner
//! `LightrBackend`, then converts the local RESULT (and any error) back to
//! canonical types. The conversions live in `convert.rs`; this file is pure
//! delegation.

use cri_canon as canon;
use lightr_cri_backend::LightrBackend;
// Bring the LOCAL `CriBackend` trait's methods into scope (anonymously, to avoid
// clashing with the canonical `CriBackend` we implement) so `self.0.<method>`
// resolves against the real backend's trait, not the canonical one.
use lightr_cri_backend::CriBackend as _;

use crate::convert::*;

/// Wraps the real backend and re-exposes it under the canonical seam trait.
pub struct Adapter(pub LightrBackend);

impl canon::CriBackend for Adapter {
    // ── sandbox plane ────────────────────────────────────────────────────────
    fn run_sandbox(&self, cfg: canon::SandboxConfig) -> canon::Result<canon::SandboxId> {
        self.0
            .run_sandbox(c2l_sandbox_cfg(cfg))
            .map(l2c_sandbox_id)
            .map_err(l2c_err)
    }

    fn stop_sandbox(&self, id: &canon::SandboxId) -> canon::Result<()> {
        self.0.stop_sandbox(&c2l_sandbox_id(id)).map_err(l2c_err)
    }

    fn remove_sandbox(&self, id: &canon::SandboxId) -> canon::Result<()> {
        self.0.remove_sandbox(&c2l_sandbox_id(id)).map_err(l2c_err)
    }

    fn sandbox_status(&self, id: &canon::SandboxId) -> canon::Result<canon::SandboxStatus> {
        self.0
            .sandbox_status(&c2l_sandbox_id(id))
            .map(l2c_sandbox_status)
            .map_err(l2c_err)
    }

    fn list_sandboxes(
        &self,
        filter: &canon::SandboxFilter,
    ) -> canon::Result<Vec<canon::SandboxStatus>> {
        self.0
            .list_sandboxes(&c2l_sandbox_filter(filter))
            .map(|v| v.into_iter().map(l2c_sandbox_status).collect())
            .map_err(l2c_err)
    }

    // ── container plane ──────────────────────────────────────────────────────
    fn create_container(
        &self,
        sandbox: &canon::SandboxId,
        cfg: canon::ContainerConfig,
    ) -> canon::Result<canon::ContainerId> {
        self.0
            .create_container(&c2l_sandbox_id(sandbox), c2l_container_cfg(cfg))
            .map(l2c_container_id)
            .map_err(l2c_err)
    }

    fn start_container(&self, id: &canon::ContainerId) -> canon::Result<()> {
        self.0.start_container(&c2l_container_id(id)).map_err(l2c_err)
    }

    fn stop_container(&self, id: &canon::ContainerId, grace_seconds: i64) -> canon::Result<()> {
        self.0
            .stop_container(&c2l_container_id(id), grace_seconds)
            .map_err(l2c_err)
    }

    fn remove_container(&self, id: &canon::ContainerId) -> canon::Result<()> {
        self.0
            .remove_container(&c2l_container_id(id))
            .map_err(l2c_err)
    }

    fn container_status(
        &self,
        id: &canon::ContainerId,
    ) -> canon::Result<canon::ContainerStatus> {
        self.0
            .container_status(&c2l_container_id(id))
            .map(l2c_container_status)
            .map_err(l2c_err)
    }

    fn list_containers(
        &self,
        filter: &canon::ContainerFilter,
    ) -> canon::Result<Vec<canon::ContainerStatus>> {
        self.0
            .list_containers(&c2l_container_filter(filter))
            .map(|v| v.into_iter().map(l2c_container_status).collect())
            .map_err(l2c_err)
    }

    fn container_stats(
        &self,
        id: &canon::ContainerId,
    ) -> canon::Result<canon::ContainerStatsRec> {
        self.0
            .container_stats(&c2l_container_id(id))
            .map(l2c_stats)
            .map_err(l2c_err)
    }

    fn list_container_stats(
        &self,
        filter: &canon::ContainerFilter,
    ) -> canon::Result<Vec<canon::ContainerStatsRec>> {
        self.0
            .list_container_stats(&c2l_container_filter(filter))
            .map(|v| v.into_iter().map(l2c_stats).collect())
            .map_err(l2c_err)
    }

    // ── exec plane ───────────────────────────────────────────────────────────
    fn exec_sync(
        &self,
        id: &canon::ContainerId,
        cmd: &[String],
        timeout_seconds: i64,
    ) -> canon::Result<canon::ExecResult> {
        // `cmd: &[String]` is std → passes through with no conversion.
        self.0
            .exec_sync(&c2l_container_id(id), cmd, timeout_seconds)
            .map(l2c_exec_result)
            .map_err(l2c_err)
    }

    // ── image plane ──────────────────────────────────────────────────────────
    fn pull_image(&self, image_ref: &str) -> canon::Result<canon::PulledImage> {
        self.0.pull_image(image_ref).map(l2c_pulled).map_err(l2c_err)
    }

    fn image_status(&self, image_ref: &str) -> canon::Result<Option<canon::ImageRecord>> {
        self.0
            .image_status(image_ref)
            .map(|o| o.map(l2c_image_record))
            .map_err(l2c_err)
    }

    fn list_images(&self) -> canon::Result<Vec<canon::ImageRecord>> {
        self.0
            .list_images()
            .map(|v| v.into_iter().map(l2c_image_record).collect())
            .map_err(l2c_err)
    }

    fn remove_image(&self, image_ref: &str) -> canon::Result<()> {
        self.0.remove_image(image_ref).map_err(l2c_err)
    }

    fn image_fs_info(&self) -> canon::Result<canon::FsInfo> {
        self.0.image_fs_info().map(l2c_fs_info).map_err(l2c_err)
    }

    // ── v1.1 streaming ───────────────────────────────────────────────────────
    fn open_exec(
        &self,
        id: &canon::ContainerId,
        cmd: &[String],
        tty: bool,
        stdin: bool,
    ) -> canon::Result<canon::StreamSession> {
        self.0
            .open_exec(&c2l_container_id(id), cmd, tty, stdin)
            .map(l2c_stream)
            .map_err(l2c_err)
    }

    fn open_attach(&self, id: &canon::ContainerId) -> canon::Result<canon::StreamSession> {
        self.0
            .open_attach(&c2l_container_id(id))
            .map(l2c_stream)
            .map_err(l2c_err)
    }

    fn pull_image_with_auth(
        &self,
        image_ref: &str,
        auth: Option<&canon::AuthConfig>,
    ) -> canon::Result<canon::PulledImage> {
        let local_auth = auth.map(c2l_auth);
        self.0
            .pull_image_with_auth(image_ref, local_auth.as_ref())
            .map(l2c_pulled)
            .map_err(l2c_err)
    }

    fn network_ready(&self) -> bool {
        self.0.network_ready()
    }
}
