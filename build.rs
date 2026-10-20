fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/app_icon.ico");

        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app_icon.ico");

        // 若使用 GNU 工具链且 PATH 中未直接找到 windres，智能探测常见安装路径作为后备
        if std::env::var("TARGET").is_ok_and(|t| t.contains("gnu")) && which_tool("windres").is_none() {
            let candidates = [
                r"C:\msys64\ucrt64\bin\windres.exe",
                r"C:\msys64\mingw64\bin\windres.exe",
                r"D:\msys64\ucrt64\bin\windres.exe",
                r"D:\msys64\mingw64\bin\windres.exe",
            ];
            for path in candidates {
                if std::path::Path::new(path).exists() {
                    res.set_windres_path(path);
                    break;
                }
            }
        }

        res.compile().expect("Failed to compile Windows resource");
    }
}

#[cfg(target_os = "windows")]
fn which_tool(tool: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let full = dir.join(format!("{tool}.exe"));
            if full.is_file() {
                Some(full)
            } else {
                let no_ext = dir.join(tool);
                if no_ext.is_file() {
                    Some(no_ext)
                } else {
                    None
                }
            }
        })
    })
}
