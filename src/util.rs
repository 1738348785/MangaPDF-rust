/// 将路径转换为 Windows `explorer.exe /select,` 要求的绝对反斜杠路径（剥离 `\\?\` 前缀并替换 `/` 为 `\`）
pub fn normalize_windows_explorer_path(path: &std::path::Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let raw = abs.to_string_lossy();
    let stripped = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    stripped.replace('/', r"\")
}

/// 在系统文件管理器中定位并高亮指定文件或目录
pub fn open_in_file_explorer(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut cmd = std::process::Command::new("explorer.exe");
        cmd.creation_flags(CREATE_NO_WINDOW);

        if path.exists() {
            let norm = normalize_windows_explorer_path(path);
            if path.is_file() {
                cmd.raw_arg(format!("/select,\"{}\"", norm));
            } else {
                cmd.raw_arg(format!("\"{}\"", norm));
            }
        } else if let Some(parent) = path.parent() {
            if parent.exists() {
                let norm = normalize_windows_explorer_path(parent);
                cmd.raw_arg(format!("\"{}\"", norm));
            }
        } else {
            let norm = normalize_windows_explorer_path(path);
            cmd.raw_arg(format!("\"{}\"", norm));
        }

        let _ = cmd.spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let target = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        #[cfg(target_os = "macos")]
        {
            if path.is_file() && path.exists() {
                let _ = std::process::Command::new("open")
                    .arg("-R")
                    .arg(path)
                    .spawn();
            } else {
                let _ = std::process::Command::new("open").arg(target).spawn();
            }
        }
        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            let _ = std::process::Command::new("xdg-open").arg(target).spawn();
        }
    }
}

/// 预检目标文件是否可被覆盖替换（在 Windows 下同时申请 DELETE 与 GENERIC_WRITE 权限，
/// 能准确捕获被 Edge/Chrome/阅读器以禁止删除方式锁住或设为只读的 PDF，且不修改原文件时间戳）
pub fn check_file_replaceable(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const DELETE: u32 = 0x0001_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        std::fs::OpenOptions::new()
            .access_mode(DELETE | GENERIC_WRITE)
            .open(path)
            .map(|_| ())
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map(|_| ())
    }
}

/// 从导入文件夹路径提取非空分卷名称（兼容直接选择盘符根目录如 `D:\` 时 `file_name()` 为空的情况）
pub fn derive_folder_sequence_name(dir: &std::path::Path) -> String {
    if let Some(name) = dir.file_name().map(|n| n.to_string_lossy()) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let raw = dir.to_string_lossy();
    let cleaned = raw
        .trim()
        .trim_end_matches(['\\', '/'])
        .trim_end_matches(':');
    if !cleaned.is_empty() {
        format!("{}盘", cleaned)
    } else {
        "根目录".to_string()
    }
}

/// 删除分卷内第 `removed` 张图片后，重映射预览窗当前页索引：
/// 删除的正是正在预览的页 → 返回 None (关闭预览)；删除点在预览页之前 → 索引前移一位。
pub fn preview_index_after_remove(cur: usize, removed: usize) -> Option<usize> {
    if cur == removed {
        None
    } else if cur > removed {
        Some(cur - 1)
    } else {
        Some(cur)
    }
}

/// 交换分卷内第 `from` 与 `to` 张图片后，重映射预览窗当前页索引（被交换的两页互换位置）。
pub fn preview_index_after_swap(cur: usize, from: usize, to: usize) -> Option<usize> {
    if cur == from {
        Some(to)
    } else if cur == to {
        Some(from)
    } else {
        Some(cur)
    }
}

/// 将第 `from` 张图片移动到第 `to` 位后，重映射预览窗当前页索引，避免预览悄悄跳到相邻页。
pub fn preview_index_after_move(cur: usize, from: usize, to: usize) -> Option<usize> {
    if cur == from {
        Some(to)
    } else if from < cur && cur <= to {
        Some(cur - 1)
    } else if to <= cur && cur < from {
        Some(cur + 1)
    } else {
        Some(cur)
    }
}

/// 智能净化 Windows 文件名，将非法字符转换为友好全角字符或合法字符，并过滤系统保留设备名
pub fn sanitize_filename(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '?' => sanitized.push('？'),       // 中文全角问号 (保留问号原意)
            ':' => sanitized.push('：'),       // 中文全角冒号
            '*' => sanitized.push('×'),        // 乘号替代星号
            '/' | '\\' => sanitized.push('-'), // 连字符替代斜杠
            '<' => sanitized.push('《'),       // 书名号替代尖括号
            '>' => sanitized.push('》'),
            '|' => sanitized.push('_'),
            '"' => sanitized.push('\''),
            c if c.is_control() => sanitized.push('_'),
            c => sanitized.push(c),
        }
    }

    let end_trimmed = sanitized.trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    let is_empty_pdf_ext = end_trimmed
        .get(end_trimmed.len().saturating_sub(4)..)
        .is_some_and(|ext| ext.eq_ignore_ascii_case(".pdf"))
        && end_trimmed[..end_trimmed.len() - 4]
            .trim_matches(|c: char| c.is_whitespace() || c == '.')
            .is_empty();
    let trimmed = if is_empty_pdf_ext {
        ""
    } else {
        end_trimmed.trim_start_matches(|c: char| c.is_whitespace() || c == '.')
    };
    // 限制文件名最大字符长度，避免深层输出目录叠加超长轻小说/漫画标题超出 Windows MAX_PATH (260)
    const MAX_FILENAME_CHARS: usize = 160;
    let mut base = if trimmed.is_empty() {
        "未命名分卷".to_string()
    } else if trimmed.chars().count() > MAX_FILENAME_CHARS {
        let has_pdf_ext = trimmed.to_ascii_lowercase().ends_with(".pdf");
        let stem_part = if has_pdf_ext {
            &trimmed[..trimmed.len() - 4]
        } else {
            trimmed
        };
        let limit = if has_pdf_ext {
            MAX_FILENAME_CHARS - 4
        } else {
            MAX_FILENAME_CHARS
        };
        let short_stem: String = stem_part.chars().take(limit).collect();
        let clean_stem = short_stem.trim_end_matches(|c: char| c.is_whitespace() || c == '.');
        if clean_stem.is_empty() {
            "未命名分卷".to_string()
        } else if has_pdf_ext {
            format!("{}.pdf", clean_stem)
        } else {
            clean_stem.to_string()
        }
    } else {
        trimmed.to_string()
    };

    // 过滤 Windows 保留设备名 (CON, PRN, AUX, NUL, COM1..9, LPT1..9)
    // Windows 对多级后缀 (如 CON.vol1.pdf) 或点号前带空格 (如 "CON .pdf") 同样视为设备名，需取首个 '.' 之前并 trim 比对
    let first_seg = base.split('.').next().unwrap_or(&base).trim();

    const RESERVED_NAMES: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];

    if RESERVED_NAMES.contains(&first_seg.to_ascii_lowercase().as_str()) {
        base.insert(0, '_');
    }

    base
}

/// 针对 Windows 进程主动修剪工作集内存，将启动阶段显卡驱动预分配的大量非活跃编译缓存归还系统
#[cfg(target_os = "windows")]
pub fn trim_process_memory() {
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn SetProcessWorkingSetSize(
                h_process: isize,
                dw_minimum_working_set_size: usize,
                dw_maximum_working_set_size: usize,
            ) -> i32;
        }
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn trim_process_memory() {}

#[cfg(target_os = "windows")]
mod win_encoding {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetACP() -> u32;
        fn MultiByteToWideChar(
            code_page: u32,
            dw_flags: u32,
            lp_multi_byte_str: *const u8,
            cb_multi_byte: i32,
            lp_wide_char_str: *mut u16,
            cch_wide_char: i32,
        ) -> i32;
    }

    const MB_ERR_INVALID_CHARS: u32 = 0x0000_0008;

    pub fn system_acp() -> u32 {
        unsafe { GetACP() }
    }

    /// 严格按指定 Windows 代码页解码字节流；若含非法多字节序列返回 None
    pub fn decode_strict(code_page: u32, bytes: &[u8]) -> Option<String> {
        if bytes.is_empty() {
            return Some(String::new());
        }
        let len = i32::try_from(bytes.len()).ok()?;
        let needed = unsafe {
            MultiByteToWideChar(
                code_page,
                MB_ERR_INVALID_CHARS,
                bytes.as_ptr(),
                len,
                std::ptr::null_mut(),
                0,
            )
        };
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0u16; needed as usize];
        let written = unsafe {
            MultiByteToWideChar(
                code_page,
                MB_ERR_INVALID_CHARS,
                bytes.as_ptr(),
                len,
                wide.as_mut_ptr(),
                needed,
            )
        };
        if written <= 0 {
            return None;
        }
        wide.truncate(written as usize);
        String::from_utf16(&wide).ok()
    }
}

#[cfg(target_os = "windows")]
fn score_decoded_zip_entry(cp: u32, raw: &[u8], decoded: &str) -> i64 {
    let mut score = 0i64;

    // 1. 基于各编码高频字符字节区间评分，精确区分 GBK 简体与 Shift-JIS 日文重叠区
    let mut i = 0usize;
    while i < raw.len() {
        let b = raw[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        match cp {
            936 | 54936 => {
                if i + 1 < raw.len() {
                    let b2 = raw[i + 1];
                    if (0xB0..=0xF7).contains(&b) && (0xA1..=0xFE).contains(&b2) {
                        score += 6; // GB2312 常用汉字区
                    } else if (0xA1..=0xA9).contains(&b) && (0xA1..=0xFE).contains(&b2) {
                        score += 4; // GB2312 全角标点与符号区
                    } else if (0x81..=0xA0).contains(&b) {
                        score -= 3; // GBK/3 生僻字区（与 Shift-JIS 高频区高度重叠）
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            932 => {
                if (0xA1..=0xDF).contains(&b) {
                    score -= 3; // 半角片假名（GBK 汉字误按 Shift-JIS 解码时的典型特征）
                    i += 1;
                } else if i + 1 < raw.len() {
                    if (0x81..=0x9F).contains(&b) {
                        score += 5; // JIS X 0208 常用假名与一级汉字区
                    } else if (0xE0..=0xEA).contains(&b) {
                        score += 2; // JIS 二级汉字区
                    } else {
                        score -= 2;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            950 => {
                if i + 1 < raw.len() {
                    if (0xA4..=0xC6).contains(&b) {
                        score += 5; // Big5 常用繁体一级字区
                    } else if (0xA1..=0xA3).contains(&b) {
                        score += 3;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                i += 2.min(raw.len() - i);
            }
        }
    }

    // 2. 基于解码后 Unicode 字符特征评分（假名/谚文具有极强语言指向性）
    for ch in decoded.chars() {
        match ch {
            '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' => score += 10, // 日文平/片假名
            '\u{AC00}'..='\u{D7A3}' => score += 10,                           // 韩文音节
            '\u{3400}'..='\u{4DBF}' | '\u{E000}'..='\u{F8FF}' | '\u{FFFD}' => score -= 10,
            c if c.is_control() => score -= 20,
            _ => {}
        }
    }

    score
}

/// 构造 ZIP 条目名编码探测的「主候选池」：
/// 简体 GBK(936) 与日文 Shift-JIS(932) 恒定参与（与系统区域设置无关），
/// 系统代码页在没有重复时追加一项，让 `pick_best` 中的 `sys_acp` 同分加权继续生效。
pub fn primary_code_page_pool(sys_acp: u32) -> Vec<u32> {
    let mut pool = vec![936u32, 932u32];
    if !pool.contains(&sys_acp) {
        pool.push(sys_acp);
    }
    pool
}

/// 批量按归档级别自动探测并解码 ZIP/CBZ 原始条目名：
/// - 合法 UTF-8（含纯 ASCII 与带 Info-ZIP Unicode 扩展字段的条目）直接保留；
/// - 针对未置 UTF-8 标志位的非 UTF-8 归档（中文 GBK/GB18030、日文 Shift-JIS、繁体 Big5、韩文 CP949），
///   在整包维度联合评估最佳编码，彻底避免乱码与 Shift-JIS/GBK 第二字节 `0x5C` (`\`) 导致的路径错切。
pub fn decode_zip_entry_names(raw_names: &[&[u8]]) -> Vec<String> {
    let mut non_utf8: Vec<&[u8]> = Vec::new();
    for &raw in raw_names {
        if std::str::from_utf8(raw).is_err() {
            non_utf8.push(raw);
        }
    }

    if non_utf8.is_empty() {
        return raw_names
            .iter()
            .map(|&raw| std::str::from_utf8(raw).unwrap_or_default().to_string())
            .collect();
    }

    #[cfg(target_os = "windows")]
    let chosen_cp: Option<u32> = {
        let sys_acp = win_encoding::system_acp();

        let pick_best = |candidates: &[u32]| -> Option<(u32, usize, i64)> {
            let mut best: Option<(u32, usize, i64)> = None;
            for &cp in candidates {
                let mut valid_count = 0usize;
                let mut total_score = if cp == sys_acp { 2i64 } else { 0i64 };
                for &raw in &non_utf8 {
                    if let Some(decoded) = win_encoding::decode_strict(cp, raw) {
                        valid_count += 1;
                        total_score += score_decoded_zip_entry(cp, raw, &decoded);
                    }
                }
                if valid_count == 0 {
                    continue;
                }
                let replace = match best {
                    None => true,
                    Some((_, best_valid, best_score)) => {
                        valid_count > best_valid
                            || (valid_count == best_valid && total_score > best_score)
                    }
                };
                if replace {
                    best = Some((cp, valid_count, total_score));
                }
            }
            best
        };

        // 主探测池：**固定**包含简体 GBK(936) 与日文 Shift-JIS(932)，系统代码页在未重复时追加参与角逐。
        // 不能因为本机是繁体/韩文系统就把 936 挤出候选：Big5 与 CP949 的字节区间完全覆盖 GB2312 常用区，
        // 一旦丢掉 936，简体中文归档会被「完整解码成功」成繁体/韩文乱码，
        // 并连带把基于名字派生的章节排序键算错，导致成品 PDF 页序错乱。
        let primary_candidates = primary_code_page_pool(sys_acp);
        let primary_best = pick_best(&primary_candidates);

        // 仅当主探测池无法 100% 完整解码所有条目时，再扩展至 GB18030 / Big5 / CP949 / 系统默认页
        if primary_best.is_some_and(|(_, valid, _)| valid == non_utf8.len()) {
            primary_best.map(|(cp, _, _)| cp)
        } else {
            let fallback_candidates = [54936, 936, 932, 950, 949, sys_acp];
            pick_best(&fallback_candidates).map(|(cp, _, _)| cp)
        }
    };

    raw_names
        .iter()
        .map(|&raw| {
            if let Ok(s) = std::str::from_utf8(raw) {
                return s.to_string();
            }
            #[cfg(target_os = "windows")]
            if let Some(cp) = chosen_cp {
                if let Some(decoded) = win_encoding::decode_strict(cp, raw) {
                    return decoded;
                }
            }
            String::from_utf8_lossy(raw).into_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_reserved_and_special_cases() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.pdf"), "_con.pdf");
        assert_eq!(sanitize_filename("CON.vol1.pdf"), "_CON.vol1.pdf");
        assert_eq!(sanitize_filename("NUL .pdf"), "_NUL .pdf");
        assert_eq!(
            sanitize_filename("第1话: 命运? <上>"),
            "第1话： 命运？ 《上》"
        );
        assert_eq!(sanitize_filename("   ...   "), "未命名分卷");
        assert_eq!(sanitize_filename(".pdf"), "未命名分卷");
        assert_eq!(sanitize_filename("   .PDF "), "未命名分卷");
        assert_eq!(sanitize_filename("...pdf"), "未命名分卷");

        let very_long = format!("{}.pdf", "漫".repeat(250));
        let sanitized = sanitize_filename(&very_long);
        assert!(sanitized.ends_with(".pdf"));
        assert!(sanitized.chars().count() <= 160);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_decode_zip_entry_names_gbk_and_shift_jis_with_0x5c() {
        // 1. GBK 编码中文归档："第1话/封面.jpg", "第1话/001.jpg"
        let gbk_1: &[u8] = &[
            0xB5, 0xDA, b'1', 0xBB, 0xB0, b'/', 0xB7, 0xE2, 0xC3, 0xE6, b'.', b'j', b'p', b'g',
        ];
        let gbk_2: &[u8] = &[0xB5, 0xDA, b'1', 0xBB, 0xB0, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'];
        let gbk_decoded = decode_zip_entry_names(&[gbk_1, gbk_2]);
        assert_eq!(gbk_decoded, vec!["第1话/封面.jpg", "第1话/001.jpg"]);

        // 2. Shift-JIS 编码日文归档（特意包含第二字节为 0x5C '\' 的「表」[0x95, 0x5C] 与「ソ」[0x83, 0x5C]）
        // "第1話/表紙.jpg", "第1話/ソラ.jpg"
        let sjis_1: &[u8] = &[
            0x91, 0xE6, b'1', 0x98, 0x62, b'/', 0x95, 0x5C, 0x8E, 0x86, b'.', b'j', b'p', b'g',
        ];
        let sjis_2: &[u8] = &[
            0x91, 0xE6, b'1', 0x98, 0x62, b'/', 0x83, 0x5C, 0x83, 0x89, b'.', b'j', b'p', b'g',
        ];
        let sjis_decoded = decode_zip_entry_names(&[sjis_1, sjis_2]);
        assert_eq!(sjis_decoded, vec!["第1話/表紙.jpg", "第1話/ソラ.jpg"]);
        assert!(
            !sjis_decoded[0].contains('\\'),
            "Shift-JIS 中含 0x5C 的「表」不应被错切为反斜杠"
        );

        // 3. 标准 UTF-8 归档保持不变
        let utf8_name = "第01話/表紙.png".as_bytes();
        assert_eq!(decode_zip_entry_names(&[utf8_name]), vec!["第01話/表紙.png"]);
    }

    #[test]
    fn test_derive_folder_sequence_name_handles_drive_roots() {
        use std::path::Path;
        assert_eq!(derive_folder_sequence_name(Path::new(r"E:\Manga\第01卷")), "第01卷");
        assert_eq!(derive_folder_sequence_name(Path::new(r"D:\")), "D盘");
        assert_eq!(derive_folder_sequence_name(Path::new("/")), "根目录");
    }

    #[test]
    fn test_primary_code_page_pool_always_includes_gbk() {
        // 简体系统：池与此前完全一致，行为不变
        assert_eq!(primary_code_page_pool(936), vec![936, 932]);
        // 日文系统：932 已在池内，不重复追加
        assert_eq!(primary_code_page_pool(932), vec![936, 932]);
        // 繁体 / 韩文 / 西欧系统：936 必须仍然参战，
        // 否则 GBK 归档会被 Big5/CP949 "完整解码成功"成乱码，连带算错章节排序
        for acp in [950u32, 949, 1252, 437] {
            let pool = primary_code_page_pool(acp);
            assert!(pool.contains(&936), "ACP={acp} 时主探测池必须包含 GBK(936)");
            assert!(
                pool.contains(&932),
                "ACP={acp} 时主探测池必须包含 Shift-JIS(932)"
            );
            assert!(pool.contains(&acp), "ACP={acp} 自身也要参与角逐");
            assert_eq!(
                pool.iter().filter(|&&cp| cp == acp).count(),
                1,
                "ACP={acp} 不应重复出现"
            );
        }
    }

    #[test]
    fn test_preview_index_remap_on_remove_swap_and_move() {
        // 删除：删掉正在预览的页 → 关闭预览；删掉前面的页 → 索引前移
        assert_eq!(preview_index_after_remove(3, 3), None);
        assert_eq!(preview_index_after_remove(4, 3), Some(3));
        assert_eq!(preview_index_after_remove(2, 3), Some(2));
        assert_eq!(preview_index_after_remove(0, 0), None);

        // 交换：被交换的两页互换位置
        assert_eq!(preview_index_after_swap(3, 3, 5), Some(5));
        assert_eq!(preview_index_after_swap(5, 3, 5), Some(3));
        assert_eq!(preview_index_after_swap(4, 3, 5), Some(4));

        // 移动：跨越区间内的索引按位移方向补偿
        assert_eq!(preview_index_after_move(2, 2, 7), Some(7));
        assert_eq!(preview_index_after_move(5, 2, 7), Some(4));
        assert_eq!(preview_index_after_move(2, 4, 1), Some(3));
        assert_eq!(preview_index_after_move(9, 2, 7), Some(9));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_check_file_replaceable_detects_readonly_target() {
        let dir = std::env::temp_dir().join(format!(
            "mangapdf_replaceable_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vol1.pdf");
        std::fs::write(&path, b"%PDF-1.7 test").unwrap();
        assert!(check_file_replaceable(&path).is_ok());

        // 备份原始权限，测试结束后原样还原（Windows 下只读文件无法直接删除）
        let original_perms = std::fs::metadata(&path).unwrap().permissions();
        let mut read_only = original_perms.clone();
        read_only.set_readonly(true);
        std::fs::set_permissions(&path, read_only).unwrap();
        assert!(
            check_file_replaceable(&path).is_err(),
            "只读 PDF 必须被判定为不可覆盖，避免白跑转码流程"
        );

        let _ = std::fs::set_permissions(&path, original_perms);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_normalize_windows_explorer_path_replaces_forward_slashes() {
        use std::path::Path;
        let norm = normalize_windows_explorer_path(Path::new("E:/Manga/out/vol1.pdf"));
        assert!(!norm.contains('/'));
        assert!(norm.ends_with(r"E:\Manga\out\vol1.pdf"));
    }
}
