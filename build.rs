use std::path::Path;
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        println!("cargo:rerun-if-changed=assets/app_icon.ico");
        println!("cargo:rerun-if-changed=app.rc");

        let out_dir = std::env::var("OUT_DIR").unwrap();
        let res_path = Path::new(&out_dir).join("app_res.o");

        let windres_cmd = [
            r"D:\msys64\ucrt64\bin\windres.exe",
            r"C:\msys64\ucrt64\bin\windres.exe",
            r"D:\msys64\mingw64\bin\windres.exe",
            r"C:\msys64\mingw64\bin\windres.exe",
            "windres.exe",
            "windres",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("windres");

        let status = Command::new(windres_cmd)
            .args(&["-i", "app.rc", "-O", "coff", "-o"])
            .arg(&res_path)
            .status()
            .expect("Failed to execute windres");

        if !status.success() {
            panic!("windres compilation failed with status: {:?}", status);
        }

        println!("cargo:rustc-link-arg={}", res_path.display());
    }
}
