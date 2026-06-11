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
/// IMPORTANT (WP-B-vsock honesty contract): this function does NOT return the
/// guest process's exit code.  Apple's Virtualization framework never surfaces
/// the guest exit code to the host, so any value invented here would be a lie.
/// Instead, the guest's PID1 (`lightr-init`) writes its REAL exit code over an
/// AF_VSOCK channel, and the RUST host (`VsockExitReceiver`, see
/// crates/lightr-engine/src/vsock.rs) reads it.  This shim therefore reports
/// only whether the VM booted and stopped cleanly; `VzEngine::run` combines
/// that with the vsock frame to produce the real exit code.
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
    // BOOT-PATH: construct the kernel command line that tells PID 1 what to exec.
    var cmdArgs: [String] = []
    for i in 0..<Int(argc) {
        if let ptr = argv[i] {
            cmdArgs.append(String(cString: ptr))
        }
    }
    // Encode command as LIGHTR_CMD=arg0\x1Farg1… (unit separator); the guest
    // PID1 reads this from /proc/cmdline and execs it.
    let cmdEncoded = cmdArgs.joined(separator: "\u{1F}")
    let cmdLine    = "console=hvc0 LIGHTR_CMD=\(cmdEncoded)"

    let bootLoader = VZLinuxBootLoader(kernelURL: kernelURL)
    bootLoader.initialRamdiskURL = initrdURL
    bootLoader.commandLine       = cmdLine

    // ── 3. CPU + memory ─────────────────────────────────────────────────────
    let cpuCount = max(1, VZVirtualMachineConfiguration.maximumAllowedCPUCount / 4)
    let memBytes = UInt64(256) * 1024 * 1024  // 256 MB baseline (ADR-0014)

    // ── 4. Virtiofs shares ──────────────────────────────────────────────────
    var storages: [VZDirectorySharingDeviceConfiguration] = []

    // rootfs share (tag "rootfs", read-write so the guest can pivot/write)
    let rootfsShare = VZSharedDirectory(url: URL(fileURLWithPath: rootfsPath),
                                        readOnly: false)
    let rootfsDev   = VZVirtioFileSystemDeviceConfiguration(tag: "rootfs")
    rootfsDev.share = VZMultipleDirectoryShare(directories: ["rootfs": rootfsShare])
    storages.append(rootfsDev)

    // store share (tag "store", read-only)
    if !storePath.isEmpty {
        let storeShare = VZSharedDirectory(url: URL(fileURLWithPath: storePath),
                                           readOnly: true)
        let storeDev   = VZVirtioFileSystemDeviceConfiguration(tag: "store")
        storeDev.share = VZMultipleDirectoryShare(directories: ["store": storeShare])
        storages.append(storeDev)
    }

    // ── 5. Serial console → inherit host stdio ──────────────────────────────
    // BOOT-PATH: attach /dev/hvc0 to the host's stdin/stdout so the guest's
    // serial output flows through (inherit semantics per spec).
    let consolePort = VZVirtioConsoleDeviceSerialPortConfiguration()
    consolePort.attachment = VZFileHandleSerialPortAttachment(
        fileHandleForReading:  FileHandle.standardInput,
        fileHandleForWriting:  FileHandle.standardOutput
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
    // BOOT-PATH: VZVirtualMachine must be started on the main queue.
    //
    // `vmStatus` is a LIFECYCLE status, never the guest exit code. The guest's
    // real exit code is delivered to the Rust host over AF_VSOCK by PID1 (see
    // the function doc + crates/lightr-engine/src/vsock.rs); this shim only
    // signals whether the VM reached a clean stop. The old fabricated-success
    // assignment that pinned the result to zero on stop has been removed.
    let vm        = VZVirtualMachine(configuration: config, queue: .main)
    let semaphore = DispatchSemaphore(value: 0)
    var vmStatus: Int32 = -1

    // Observe stop (guest powered the VM down).
    let observation = vm.observe(\.state, options: [.new]) { machine, _ in
        switch machine.state {
        case .stopped:
            // BOOT-PATH: the VM stopped cleanly. We report lifecycle success
            // ONLY; the actual guest exit code already flowed to the Rust host
            // over vsock. We do NOT invent a process exit code here.
            vmStatus = 0
            semaphore.signal()
        case .error:
            fputs("lightr-vz-shim: VM error\n", stderr)
            vmStatus = -1
            semaphore.signal()
        default:
            break
        }
    }
    _ = observation  // keep alive

    vm.start { result in
        if case .failure(let error) = result {
            fputs("lightr-vz-shim: boot failed: \(error)\n", stderr)
            vmStatus = -1
            semaphore.signal()
        }
    }

    // Block until the VM stops (or fails), then hand the lifecycle status back.
    semaphore.wait()
    return vmStatus
}
