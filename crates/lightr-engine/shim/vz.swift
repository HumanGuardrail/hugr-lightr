// shim/vz.swift — VzEngine Swift shim for lightr-engine (build-spec-r2 §2).
//
// Compiled to a static lib by build.rs ONLY when feature "vz" is enabled.
// Default builds never reach this file.
//
// S5 BOOT NOTE: The actual microVM boot path has not been validated on Intel
// x86_64 (Apple VZ save/restore is arm64-only; cold-boot with a suitable
// kernel pack may work on x86 but is owner-spike S5 territory). The Swift
// code below is architecturally complete and compiles against the
// Virtualization framework; the boot path is marked with // BOOT-PATH
// comments for the S5 reviewer.
//
// Exported C symbol: lightr_vz_run
// Matches extern "C" in crates/lightr-engine/src/lib.rs vz_impl module.

import Foundation
import Virtualization

/// Boot a Linux microVM, run the supplied command as the guest init, and
/// report the VM's LIFECYCLE status.  Called from Rust via C ABI.
///
/// IMPORTANT (WP-B honesty contract): this function does NOT return the guest
/// process's exit code.  Apple's Virtualization framework never surfaces the
/// guest exit code to the host, so any value invented here would be a lie — and
/// macOS has no host AF_VSOCK to carry it either.  Instead, the guest's PID1
/// (`lightr-init`) writes its REAL exit code to a file on the shared (writable)
/// rootfs (`.lightr-exit-code`), and the RUST host (`VzEngine::run`) reads it
/// back after the VM stops.  This shim therefore reports only whether the VM
/// booted and stopped cleanly; `VzEngine::run` combines that with the rootfs
/// exit file to produce the real exit code.
///
/// - Parameters:
///   - kernel:  NUL-terminated path to a Linux kernel image (vmlinuz / bzImage).
///   - initrd:  NUL-terminated path to an initrd/initramfs file.
///   - rootfs:  NUL-terminated path to the CoW rootfs directory to share via
///               virtiofs at guest tag "rootfs".
///   - store:   NUL-terminated path to the read-only store directory to share
///               at guest tag "store".  Pass "" to skip the store share.
///   - argc:    Number of arguments in argv.
///   - argv:    C argv array (argv[0] = program, …).
///
/// - Returns: VM-lifecycle status — `0` = booted and stopped cleanly,
///            `-1` = configuration / boot failure.  NEVER the guest exit code.
@_cdecl("lightr_vz_run")
public func lightr_vz_run(
    kernel:  UnsafePointer<CChar>,
    initrd:  UnsafePointer<CChar>,
    rootfs:  UnsafePointer<CChar>,
    store:   UnsafePointer<CChar>,
    argc:    Int32,
    argv:    UnsafePointer<UnsafePointer<CChar>?>
) -> Int32 {

    // ── 1. Paths ────────────────────────────────────────────────────────────
    let kernelURL  = URL(fileURLWithPath: String(cString: kernel))
    let initrdURL  = URL(fileURLWithPath: String(cString: initrd))
    let rootfsPath = String(cString: rootfs)
    let storePath  = String(cString: store)

    // ── 2. Linux bootloader ─────────────────────────────────────────────────
    // The kernel must be an x86_64 bzImage (VZ on Intel boots via the x86 setup
    // header / real-mode protocol; a raw vmlinux ELF — even a PVH one — is
    // rejected with an "Internal Virtualization error"). console=hvc0 is the
    // virtio console VZ exposes (the guest's only console). The command travels
    // via the file channel (CMD_FILE on the rootfs share), NOT the kernel
    // cmdline; argv is ignored here (kept in the C ABI for forward-compat).
    _ = argc
    _ = argv
    let cmdLine    = "console=hvc0"

    let bootLoader = VZLinuxBootLoader(kernelURL: kernelURL)
    bootLoader.initialRamdiskURL = initrdURL
    bootLoader.commandLine       = cmdLine

    // ── 3. CPU + memory ─────────────────────────────────────────────────────
    let cpuCount = max(1, VZVirtualMachineConfiguration.maximumAllowedCPUCount / 4)
    let memBytes = UInt64(256) * 1024 * 1024  // 256 MB baseline (ADR-0014)

    // ── 4. Virtiofs shares ──────────────────────────────────────────────────
    var storages: [VZDirectorySharingDeviceConfiguration] = []

    // rootfs share (tag "rootfs", read-write so the guest can pivot/write).
    // SINGLE-directory share: the directory's CONTENTS appear directly at the
    // guest mountpoint. A MultipleDirectoryShare would nest them under a
    // subdirectory named after the key (guest saw /newroot/rootfs/… instead of
    // /newroot/… — the cause of an early read_spec ENOENT).
    let rootfsShare = VZSharedDirectory(url: URL(fileURLWithPath: rootfsPath),
                                        readOnly: false)
    let rootfsDev   = VZVirtioFileSystemDeviceConfiguration(tag: "rootfs")
    rootfsDev.share = VZSingleDirectoryShare(directory: rootfsShare)
    storages.append(rootfsDev)

    // store share (tag "store", read-only) — same single-directory semantics.
    if !storePath.isEmpty {
        let storeShare = VZSharedDirectory(url: URL(fileURLWithPath: storePath),
                                           readOnly: true)
        let storeDev   = VZVirtioFileSystemDeviceConfiguration(tag: "store")
        storeDev.share = VZSingleDirectoryShare(directory: storeShare)
        storages.append(storeDev)
    }

    // ── 5. Serial console → host stdio (or a durable file for diagnosis) ─────
    // BOOT-PATH: attach /dev/hvc0 to the host. Normally writes flow to stdout
    // (inherit semantics). When LIGHTR_VZ_CONSOLE is set, the guest console is
    // captured to that file instead — durable across a SIGTERM/timeout and free
    // of any stdout/pipe/tty ambiguity (used to debug a silent boot).
    let consoleWrite: FileHandle
    if let p = ProcessInfo.processInfo.environment["LIGHTR_VZ_CONSOLE"], !p.isEmpty {
        FileManager.default.createFile(atPath: p, contents: nil)
        consoleWrite = FileHandle(forWritingAtPath: p) ?? FileHandle.standardOutput
    } else {
        consoleWrite = FileHandle.standardOutput
    }
    let consolePort = VZVirtioConsoleDeviceSerialPortConfiguration()
    consolePort.attachment = VZFileHandleSerialPortAttachment(
        fileHandleForReading:  FileHandle.standardInput,
        fileHandleForWriting:  consoleWrite
    )

    // ── 6. Assemble configuration ───────────────────────────────────────────
    let config = VZVirtualMachineConfiguration()
    config.bootLoader   = bootLoader
    config.cpuCount     = cpuCount
    config.memorySize   = memBytes
    config.serialPorts  = [consolePort]
    config.directorySharingDevices = storages

    do {
        try config.validate()
    } catch {
        fputs("lightr-vz-shim: configuration invalid: \(error)\n", stderr)
        return -1
    }

    // ── 7. Boot + wait ──────────────────────────────────────────────────────
    // `vmStatus` is a LIFECYCLE status, never the guest exit code. The guest's
    // real exit code is delivered to the Rust host as a file on the shared
    // rootfs by PID1 (see the function doc + VzEngine::run); this shim only
    // signals whether the VM reached a clean stop. The old fabricated-success
    // assignment that pinned the result to zero on stop has been removed.
    //
    // CONCURRENCY (critical): the VM runs on a DEDICATED serial queue, NOT the
    // main queue. VZ delivers `.state` transitions and the `start` completion
    // handler ON the VM's own queue. If that queue were the main queue AND we
    // block the calling thread on a semaphore (below), those callbacks could
    // never run — the VM wedges in `.starting` forever (observed empirically:
    // state -> 4 and the completion handler never fires). With a dedicated
    // queue, the calling thread blocks on the semaphore while VZ's queue keeps
    // servicing the VM all the way to `.stopped`.
    let vmQueue   = DispatchQueue(label: "com.hugr.lightr.vz")
    let semaphore = DispatchSemaphore(value: 0)
    var vmStatus: Int32 = -1
    var vm: VZVirtualMachine?
    var observation: NSKeyValueObservation?
    // Trace every lifecycle transition only when the console is being captured
    // (LIGHTR_VZ_CONSOLE set = debug); quiet otherwise. Real errors always log.
    let trace = !(ProcessInfo.processInfo.environment["LIGHTR_VZ_CONSOLE"] ?? "").isEmpty

    vmQueue.async {
        let machine = VZVirtualMachine(configuration: config, queue: vmQueue)
        vm = machine
        observation = machine.observe(\.state, options: [.new]) { m, _ in
            if trace { fputs("lightr-vz-shim: vm.state -> \(m.state.rawValue)\n", stderr) }
            switch m.state {
            case .stopped:
                // BOOT-PATH: clean stop. Lifecycle success ONLY; the real guest
                // exit code is on the shared rootfs (PID1 wrote .lightr-exit-code).
                vmStatus = 0
                semaphore.signal()
            case .error:
                fputs("lightr-vz-shim: VM entered error state\n", stderr)
                vmStatus = -1
                semaphore.signal()
            default:
                break
            }
        }
        machine.start { result in
            if case .failure(let error) = result {
                fputs("lightr-vz-shim: boot failed: \(error)\n", stderr)
                vmStatus = -1
                semaphore.signal()
            }
        }
    }

    // Block the CALLING thread (never vmQueue) until the VM stops or fails.
    semaphore.wait()
    _ = vm           // keep the VM + observation alive until the wait returns
    _ = observation
    return vmStatus
}
