//! Linux: whether to turn WebKitGTK's DMABUF renderer off before the webview
//! starts.
//!
//! On VMs and software GPUs the DMABUF renderer mis-composites (blank areas, the
//! window's client-side min/close buttons painted behind the webview), so Charon
//! disables it there. On real GPUs it must stay on: with it off, WebKitGTK 2.54
//! on Fedora 44 / Intel i915 / Wayland stopped presenting frames until an input
//! event (output and timers updated the page, the screen didn't). Found with
//! `CHARON_BISECT=dmabuf`. An explicit `WEBKIT_DISABLE_DMABUF_RENDERER` in the
//! environment always wins.

/// What the machine looks like, as far as this decision goes.
#[derive(Debug, Default, Clone, Copy)]
pub struct Gfx {
    /// A hypervisor (CPU flag or DMI vendor).
    pub vm: bool,
    /// A GPU render node (`/dev/dri/renderD*`).
    pub render_node: bool,
}

/// Disable DMABUF? `user_set`: the variable is already in the environment.
pub fn disable_dmabuf(user_set: bool, gfx: Gfx) -> bool {
    !user_set && (gfx.vm || !gfx.render_node)
}

const VM_VENDORS: &[&str] = &[
    "qemu",
    "vmware",
    "virtualbox",
    "innotek",
    "xen",
    "microsoft corporation",
    "parallels",
    "bochs",
    "red hat",
];

/// From `/proc/cpuinfo` and `/sys/class/dmi/id/sys_vendor` text.
pub fn looks_virtual(cpuinfo: &str, dmi_vendor: &str) -> bool {
    let flagged = cpuinfo
        .lines()
        .any(|l| l.starts_with("flags") && l.split_whitespace().any(|f| f == "hypervisor"));
    let v = dmi_vendor.trim().to_ascii_lowercase();
    flagged || VM_VENDORS.iter().any(|x| v.starts_with(x))
}

#[cfg(target_os = "linux")]
pub fn detect() -> Gfx {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let render_node = std::fs::read_dir("/dev/dri")
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        })
        .unwrap_or(false);
    Gfx {
        vm: looks_virtual(
            &read("/proc/cpuinfo"),
            &read("/sys/class/dmi/id/sys_vendor"),
        ),
        render_node,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_vms_and_gpu_less_machines_lose_dmabuf() {
        let real = Gfx {
            vm: false,
            render_node: true,
        };
        assert!(!disable_dmabuf(false, real));
        assert!(disable_dmabuf(
            false,
            Gfx {
                vm: true,
                render_node: true
            }
        ));
        assert!(disable_dmabuf(
            false,
            Gfx {
                vm: false,
                render_node: false
            }
        ));
        // The user's own setting is left alone either way.
        assert!(!disable_dmabuf(
            true,
            Gfx {
                vm: true,
                render_node: false
            }
        ));
    }

    #[test]
    fn spots_hypervisors() {
        assert!(looks_virtual(
            "processor : 0\nflags\t\t: fpu vme hypervisor lm\n",
            ""
        ));
        assert!(looks_virtual("flags : fpu\n", "QEMU\n"));
        assert!(looks_virtual("", "innotek GmbH"));
        assert!(!looks_virtual(
            "flags\t\t: fpu vme sse2 lm\n",
            "Dell Inc.\n"
        ));
        // "hypervisor" elsewhere in cpuinfo isn't the flag.
        assert!(!looks_virtual(
            "model name : hypervisor-ready cpu\nflags : fpu\n",
            "LENOVO"
        ));
    }
}
