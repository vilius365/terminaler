fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        use std::io::Write;
        use std::path::Path;
        let profile = std::env::var("PROFILE").unwrap();
        let repo_dir = std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.parent().map(|p| p.to_path_buf()))
            .unwrap();
        let target_triple = std::env::var("TARGET").unwrap_or_default();
        let host_triple = std::env::var("HOST").unwrap_or_default();
        let cross_compiling = !target_triple.is_empty() && target_triple != host_triple;
        let exe_output_dir = if cross_compiling {
            repo_dir.join("target").join(&target_triple).join(&profile)
        } else {
            repo_dir.join("target").join(&profile)
        };
        let windows_dir = repo_dir.join("assets").join("windows");

        // Copy companion DLLs next to the exe (only when building natively on Windows)
        if !cross_compiling {
            let conhost_dir = windows_dir.join("conhost");
            for name in &["conpty.dll", "OpenConsole.exe"] {
                let dest_name = exe_output_dir.join(name);
                let src_name = conhost_dir.join(name);
                if !dest_name.exists() {
                    let _ = std::fs::copy(&src_name, &dest_name);
                }
            }

            let angle_dir = windows_dir.join("angle");
            for name in &["libEGL.dll", "libGLESv2.dll"] {
                let dest_name = exe_output_dir.join(name);
                let src_name = angle_dir.join(name);
                if !dest_name.exists() {
                    let _ = std::fs::copy(&src_name, &dest_name);
                }
            }

            let dest_mesa = exe_output_dir.join("mesa");
            let _ = std::fs::create_dir(&dest_mesa);
            let dest_name = dest_mesa.join("opengl32.dll");
            let src_name = windows_dir.join("mesa").join("opengl32.dll");
            if !dest_name.exists() {
                let _ = std::fs::copy(&src_name, &dest_name);
            }
        }

        // Copy WebView2Loader.dll next to the exe (needed for wry/WebView2 sidebar).
        // The DLL is checked into assets/windows/ so it works for both native and cross-compile.
        {
            let src = windows_dir.join("WebView2Loader.dll");
            let dest = exe_output_dir.join("WebView2Loader.dll");
            if src.exists() && !dest.exists() {
                let _ = std::fs::copy(&src, &dest);
            }
        }

        // Version string from .tag file or git
        let mut ci_tag = String::new();
        if let Ok(tag) = std::fs::read("../.tag") {
            if let Ok(s) = String::from_utf8(tag) {
                ci_tag = s.trim().to_string();
                println!("cargo:rerun-if-changed=../.tag");
            }
        }
        let version = if ci_tag.is_empty() {
            let mut cmd = std::process::Command::new("git");
            cmd.args(&[
                "-c",
                "core.abbrev=8",
                "show",
                "-s",
                "--format=%cd-%h",
                "--date=format:%Y%m%d-%H%M%S",
            ]);
            if let Ok(output) = cmd.output() {
                if output.status.success() {
                    String::from_utf8_lossy(&output.stdout).trim().to_owned()
                } else {
                    "UNKNOWN".to_owned()
                }
            } else {
                "UNKNOWN".to_owned()
            }
        } else {
            ci_tag
        };

        // Embed Windows resource (icon + version info + manifest)
        let rcfile_name = Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("resource.rc");
        let mut rcfile = std::fs::File::create(&rcfile_name).unwrap();
        println!("cargo:rerun-if-changed=../assets/windows/terminal.ico");

        // Use forward slashes — works with both MSVC rc.exe and MinGW windres
        let win_path = windows_dir.display().to_string().replace('\\', "/");
        write!(
            rcfile,
            r#"
#include <winres.h>
// This ID is coupled with code in window/src/os/windows/window.rs
#define IDI_ICON 0x101
1 RT_MANIFEST "{win}/manifest.manifest"
IDI_ICON ICON "{win}/terminal.ico"
VS_VERSION_INFO VERSIONINFO
FILEVERSION     1,0,0,0
PRODUCTVERSION  1,0,0,0
FILEFLAGSMASK   VS_FFI_FILEFLAGSMASK
FILEFLAGS       0
FILEOS          VOS__WINDOWS32
FILETYPE        VFT_APP
FILESUBTYPE     VFT2_UNKNOWN
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904E4"
        BEGIN
            VALUE "CompanyName",      "Terminaler Contributors\0"
            VALUE "FileDescription",  "Terminaler - Terminal Multiplexer\0"
            VALUE "FileVersion",      "{version}\0"
            VALUE "LegalCopyright",   "MIT License\0"
            VALUE "InternalName",     "terminaler-gui\0"
            VALUE "OriginalFilename", "terminaler-gui.exe\0"
            VALUE "ProductName",      "Terminaler\0"
            VALUE "ProductVersion",   "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1252
    END
END
"#,
            win = win_path,
            version = version,
        )
        .unwrap();
        drop(rcfile);

        // Obtain MSVC environment so that the rc compiler can find the right headers.
        // Only needed when building natively on Windows with MSVC toolchain.
        if !cross_compiling {
            if let Some(tool) = cc::windows_registry::find_tool(target_triple.as_str(), "cl.exe") {
                for (key, value) in tool.env() {
                    std::env::set_var(key, value);
                }
            }
        }
        embed_resource::compile(rcfile_name);
    }
}
