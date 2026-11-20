/// 剥离 Windows 扩展路径前缀（`\\?\` / `\\?\UNC\`）并统一成正斜杠以外的原生分隔符。
///
/// 注意 `\\?\UNC\server\share\a.pdf` 必须还原成 `\\server\share\a.pdf`：
/// 如果只按 `\\?\` 裸剥，会得到 `UNC\server\share\a.pdf` 这种非法路径，explorer 直接打不开。
pub fn strip_windows_verbatim_prefix(raw: &str) -> String {
    let stripped = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw.to_string()
    };
    stripped.replace('/', r"\")
}

/// 将路径转换为 Windows `explorer.exe /select,` 要求的绝对反斜杠路径（剥离 `\\?\` 前缀并替换 `/` 为 `\`）
pub fn normalize_windows_explorer_path(path: &std::path::Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    strip_windows_verbatim_prefix(&abs.to_string_lossy())
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

/// 取路径的父目录，并把「空路径」视为 `None`。
///
/// `Path::new("vol1.cbz").parent()` 返回的是 `Some("")` 而不是 `None`，直接拿去当输出目录
/// 会把「输出保存目录」覆盖成空串，最终导出悄悄跑到桌面；`Path::new(".").parent()` 同理。
pub fn parent_dir_non_empty(path: &std::path::Path) -> Option<std::path::PathBuf> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
}

/// 从导入文件夹路径提取非空分卷名称（兼容直接选择盘符根目录如 `D:\` 时 `file_name()` 为空的情况）
pub fn derive_folder_sequence_name(dir: &std::path::Path) -> String {
    // 相对路径（命令行传入的 `.` / `vol1` 等）以及含 `..` 的路径的 `file_name()` 语义没有意义：
    // `Path::new(".")`、`Path::new(r"E:\Manga\sub\..")` 的 `file_name()` 都是 None，
    // 会一路掉进下面的盘符分支生成 `".盘"` 或 `"E:\Manga\sub\..盘"` 这种诡异分卷名。
    // 先转绝对路径再取名；已有根且不含 `..` 的路径（`D:\`、`/`、UNC）保持原样，避免把盘符根变成 `E盘`+数字。
    let owned;
    let dir = if dir.has_root()
        && !dir
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        dir
    } else {
        owned = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        owned.as_path()
    };

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
    if cleaned.len() == 1 && cleaned.as_bytes()[0].is_ascii_alphabetic() {
        format!("{}盘", cleaned)
    } else if !cleaned.is_empty() {
        cleaned.trim_start_matches(['\\', '/']).to_string()
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
/// 判断韩文 Hangul 音节 (`U+AC00..=U+D7A3`) 是否属于现代韩语高频常见音节构成：
/// GB2312 常用区 `0xB0..=0xC8` 与 CP949 谚文区 100% 重合，随机 GBK 字节解成韩文时
/// 极易产生冷僻重辅音 (`ㄸ/ㅃ/ㅉ`)、带韵尾罕见元音 (`룐/얩/랸/튠/듕`) 或复合/送气韵尾 (`뺐/붚`)。
/// 通过拆分初声/中声/终声排除这些低频假阳性组合，既防止 GBK 被 CP949 抢走，也不误伤真实韩文命名。
fn is_common_hangul_syllable(ch: char) -> bool {
    let idx = (ch as u32) - 0xAC00;
    let cho = idx / 588;
    let jung = (idx % 588) / 28;
    let jong = idx % 28;
    let is_rare_cho = matches!(cho, 4 | 8 | 13); // ㄸ / ㅃ / ㅉ
    let is_rare_vowel = matches!(jung, 3 | 10 | 15) // ㅒ / ㅙ / ㅞ
        || (jong != 0 && matches!(jung, 3 | 7 | 10 | 12 | 15 | 16 | 17 | 19));
    let is_rare_coda = matches!(
        jong,
        2 | 3 | 5 | 6 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 18 | 20 | 24 | 25 | 26
    );
    !(is_rare_cho || is_rare_vowel || is_rare_coda)
}

/// 归档内单个条目名的原始字节，以及归档自身是否已经声明它是 UTF-8。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawZipName<'a> {
    /// ZIP 中央目录里记录的名字原始字节
    pub raw: &'a [u8],
    /// 该条目已带 UTF-8 标志（通用位标志第 11 位），或已生效 Info-ZIP `0x7075` Unicode Path 扩展字段。
    ///
    /// 这类条目必须无条件按 UTF-8 解释。GBK 里 `目录`(0xC4 0xBF 0xC2 0xBC)、`页`(0xD2 0xB3)、
    /// `图`(0xCD 0xBC) 这些高频名字的字节本身就是合法的 2 字节 UTF-8 序列，一旦按「能 utf8 就 utf8」
    /// 短路，同一目录会被拆成 `Ŀ¼/` 与 `目录/` 两个名字，相对排序键错乱，最终成品 PDF 的页序也跟着乱。
    pub utf8: bool,
}

impl<'a> RawZipName<'a> {
    pub const fn new(raw: &'a [u8], utf8: bool) -> Self {
        Self { raw, utf8 }
    }
}

/// 候选编码：UTF-8 本身也作为候选，用于纠正「没设 UTF-8 标志、但内容确实是 UTF-8」的归档。
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ZipNameCodec {
    Utf8,
    CodePage(u32),
}

/// 判断 2 字节 UTF-8 字符的第二字节是否落在 `0x80..=0xA0`（GB2312 合法区间 `0xA1..=0xFE` 之外）。
/// 这些字节在 GBK 里属于极罕见的 GBK/4 扩展区，在真实中文文件名中几乎不会出现，
/// 却是 UTF-8 欧文大写重音（`À..Ö`/`Ü`/`à`）、俄文半数字母（`А..Р`/`р..я`）与希腊字母的高频字节。
#[cfg(target_os = "windows")]
fn is_utf8_2byte_outside_gb2312(ch: char) -> bool {
    let mut buf = [0u8; 4];
    let bytes = ch.encode_utf8(&mut buf).as_bytes();
    bytes.len() == 2 && bytes[1] <= 0xA0
}

#[cfg(target_os = "windows")]
fn is_cjk_code_page(cp: u32) -> bool {
    matches!(cp, 932 | 936 | 949 | 950 | 54936)
}

/// 现代欧洲语言与日文罗马音常见拉丁字母（排除 `×`/`÷` 运算符与极罕见/易撞 GBK 的 `Ŀ`/`ŀ`/`ſ`/`ĸ` 等）
#[cfg(target_os = "windows")]
fn is_common_european_latin(ch: char) -> bool {
    matches!(
        ch,
        // Latin-1 Supplement 字母：À..Ö, Ø..ö, ø..ÿ（法/德/西/葡/意/北欧等）
        '\u{00C0}'..='\u{00D6}'
            | '\u{00D8}'..='\u{00F6}'
            | '\u{00F8}'..='\u{00FF}'
            // Latin Extended-A 常用字母（日文罗马音长音、波兰/捷克/匈牙利/土耳其/法语 Œ/波罗的海等）
            // 刻意排除 U+013F (`Ŀ`，恰为 GBK「目」0xC4 0xBF 的 UTF-8 误读)
            | '\u{0100}'..='\u{0107}' // Ā ā Ă ă Ą ą Ć ć
            | '\u{010C}'..='\u{0113}' // Č č Ď ď Đ đ Ē ē
            | '\u{0116}'..='\u{011B}' // Ė ė Ę ę Ě ě
            | '\u{011E}'..='\u{011F}' // Ğ ğ
            | '\u{0122}'..='\u{0123}' // Ģ ģ
            | '\u{012A}'..='\u{012B}' // Ī ī
            | '\u{012E}'..='\u{0131}' // Į į İ ı
            | '\u{0136}'..='\u{0137}' // Ķ ķ
            | '\u{0139}'..='\u{013E}' // Ĺ ĺ Ļ ļ Ľ ľ
            | '\u{0141}'..='\u{0148}' // Ł ł Ń ń Ņ ņ Ň ň
            | '\u{014C}'..='\u{014D}' // Ō ō
            | '\u{0150}'..='\u{015B}' // Ő ő Œ œ Ŕ ŕ Ŗ ŗ Ř ř Ś ś
            | '\u{015E}'..='\u{0165}' // Ş ş Š š Ţ ţ Ť ť
            | '\u{016A}'..='\u{016B}' // Ū ū
            | '\u{016E}'..='\u{017E}' // Ů ů Ű ű Ų ų Ŵ ŵ Ŷ ŷ Ÿ Ź ź Ż ż Ž ž
            // 越南语带角拉丁字母 & 罗马尼亚语下逗号字母
            | '\u{01A0}'..='\u{01A1}' // Ơ ơ
            | '\u{01AF}'..='\u{01B0}' // Ư ư
            | '\u{0218}'..='\u{021B}' // Ș ș Ț ț
    )
}

/// 现代西里尔字母（俄/乌/白俄/保加利亚/塞尔维亚/马其顿等）。
/// 刻意排除 `U+0460..=U+04FF`（古教会斯拉夫语与少数民族扩展区），
/// 因为 GBK 的 `页`(0xD2 0xB3 -> `ҳ`)、`选`(0xD1 0xA1 -> `ѡ`) 等高频字在 UTF-8 下全部落入该扩展区。
#[cfg(target_os = "windows")]
fn is_modern_cyrillic(ch: char) -> bool {
    matches!(ch, '\u{0400}'..='\u{045F}')
}

/// 现代希腊字母（排除古希腊 `U+0370..=U+037F` 与科普特 `U+03CF..=U+03FF`，防止 GBK `图`=`ͼ` 误判）
#[cfg(target_os = "windows")]
fn is_modern_greek(ch: char) -> bool {
    matches!(ch, '\u{0386}'..='\u{03CE}')
        && !matches!(ch, '\u{0387}' | '\u{038B}' | '\u{038D}' | '\u{03A2}')
}

#[cfg(target_os = "windows")]
fn is_modern_armenian(ch: char) -> bool {
    matches!(ch, '\u{0531}'..='\u{0556}' | '\u{0561}'..='\u{0587}')
}

#[cfg(target_os = "windows")]
fn is_modern_hebrew(ch: char) -> bool {
    matches!(ch, '\u{05D0}'..='\u{05EA}')
}

#[cfg(target_os = "windows")]
fn is_modern_arabic(ch: char) -> bool {
    matches!(ch, '\u{0621}'..='\u{064A}' | '\u{0671}'..='\u{06D3}')
}

/// 判断区分大小写的词（西里尔/希腊/亚美尼亚）是否大小写自然：全小写、全大写或首字母大写。
/// 这能直接识破 CP1251 误读 UTF-8 俄文产生的交替大小写（如 `Том` -> `РўРѕРј`）与 GBK 撞出的 `иС`。
#[cfg(target_os = "windows")]
fn is_case_coherent_bicameral_word(word: &[char]) -> bool {
    let all_lower = word.iter().all(|c| c.is_lowercase());
    let all_upper = word.iter().all(|c| c.is_uppercase());
    let title_case = word.len() >= 2
        && word[0].is_uppercase()
        && word[1..].iter().all(|c| c.is_lowercase());
    all_lower || all_upper || title_case
}

/// 判断带重音拉丁字母在含 ASCII 字母的拉丁词中是否符合自然大小写形态：
/// - 放行正常词形态（`Café`、`CAFÉ`、`Pokémon`、`PokéBall`、`Épisode`、`Über`、`SuperÉpisode`）
/// - 拒绝 CP1252 误读 UTF-8（`CafÃ©` 中的尾部孤立大写 `Ã`）与 GBK 紧贴大写缩写（`CG猫` -> `CGè`）
#[cfg(target_os = "windows")]
fn is_latin_char_coherent_in_word(word: &[char], idx: usize) -> bool {
    let ch = word[idx];
    if ch.is_uppercase() {
        if word.iter().all(|c| c.is_uppercase()) {
            return true;
        }
        let next_is_lower = word.get(idx + 1).is_some_and(|c| c.is_lowercase());
        if idx == 0 {
            next_is_lower
        } else {
            word[idx - 1].is_lowercase() && next_is_lower
        }
    } else {
        let after_multi_upper =
            idx >= 2 && word[idx - 1].is_uppercase() && word[idx - 2].is_uppercase();
        let between_upper = idx > 0
            && word[idx - 1].is_uppercase()
            && word.get(idx + 1).is_some_and(|c| c.is_uppercase());
        !(after_multi_upper || between_upper)
    }
}

/// 单字节区间打分：利用各 CJK 代码页的高频字区分布，区分字节区间高度重叠的几种编码。
#[cfg(target_os = "windows")]
fn score_zip_name_bytes(cp: u32, raw: &[u8]) -> i64 {
    let mut score = 0i64;
    let mut i = 0usize;
    while i < raw.len() {
        let b = raw[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        // Shift-JIS 的单字节半角片假名区：GBK 汉字被误按 Shift-JIS 解码时的典型特征
        if cp == 932 && (0xA1..=0xDF).contains(&b) {
            score -= 3;
            i += 1;
            continue;
        }
        let Some(&b2) = raw.get(i + 1) else {
            break;
        };
        // GB18030 四字节序列（0x81..=0xFE, 0x30..=0x39, 0x81..=0xFE, 0x30..=0x39）：
        // 按 4 字节步进，避免把后两字节误当成双字节扩展区重复扣分
        if cp == 54936 && (0x81..=0xFE).contains(&b) && (0x30..=0x39).contains(&b2) {
            if i + 3 < raw.len() {
                i += 4;
                continue;
            }
            break;
        }
        match cp {
            // GB2312：一级汉字按拼音排在 0xB0..=0xD7，二级汉字排在 0xD8..=0xF7，
            // 0xA1..=0xA9 是全角标点与假名行；
            // GBK/3 (0x81..=0xA0) 与 GBK/4 (0xAA..=0xFE, 0x40..=0xA0) 均为生僻古字扩展区，
            // 真实简体文件名极少落入，而 UTF-8 俄文/欧文/希腊文及 Big5/Shift-JIS 的低尾字节大量命中该区
            936 | 54936 => match (b, b2) {
                (0xB0..=0xD7, 0xA1..=0xFE) => score += 6,
                (0xD8..=0xF7, 0xA1..=0xFE) => score += 2,
                (0xA1..=0xA9, 0xA1..=0xFE) => score += 3,
                (0xA1..=0xA9, 0x40..=0xA0) => score -= 2,
                (0x81..=0xA0, _) => score -= 3,
                (0xAA..=0xFE, 0x40..=0xA0) => score -= 3,
                (0xAA..=0xAF, 0xA1..=0xFE) => score -= 1,
                _ => {}
            },
            // Shift-JIS：0x81..=0x9F 是 JIS X 0208 常用假名与一级汉字，0xE0..=0xEA 是二级汉字
            932 => {
                if (0x81..=0x9F).contains(&b) {
                    score += 5;
                } else if (0xE0..=0xEA).contains(&b) {
                    score += 2;
                } else {
                    score -= 2;
                }
            }
            // Big5：一级常用字排在 0xA4..=0xC6，二级字排在 0xC9..=0xF9
            950 => {
                if (0xA4..=0xC6).contains(&b) {
                    score += 5;
                } else if (0xC9..=0xF9).contains(&b) {
                    score += 1;
                } else if (0x81..=0xA3).contains(&b) {
                    score -= 2;
                }
            }
            _ => {}
        }
        i += 2;
    }
    score
}

/// 为同书写的区分大小写词段（现代西里尔/希腊/亚美尼亚）打分。
/// `glued_to_ascii_left` / `glued_to_ascii_right` 用于识破单字节非拉丁代码页（如 CP1251/CP1253）
/// 将 UTF-8 欧文重音字节误解后嵌在英文单词内部的乱码（例如 `Shōnen` 在 CP1251 下变成 `ShЕЌnen`）。
#[cfg(target_os = "windows")]
fn score_bicameral_word(
    codec: ZipNameCodec,
    run: &[char],
    glued_to_ascii_left: bool,
    glued_to_ascii_right: bool,
) -> i64 {
    let len = run.len() as i64;
    let glued_to_ascii = glued_to_ascii_left || glued_to_ascii_right;
    let sandwiched_by_ascii = glued_to_ascii_left && glued_to_ascii_right;
    match codec {
        ZipNameCodec::Utf8 => {
            if sandwiched_by_ascii {
                return -4 * len;
            }
            if run.len() == 1 {
                if glued_to_ascii {
                    -4
                } else if is_utf8_2byte_outside_gb2312(run[0]) {
                    2
                } else {
                    -2
                }
            } else if is_case_coherent_bicameral_word(run) {
                let all_upper = run.iter().all(|c| c.is_uppercase());
                if glued_to_ascii && (run.len() < 3 || all_upper) {
                    -4 * len
                } else if run.len() >= 3 || !all_upper {
                    // 2 字节 UTF-8 词按每字符 +10（+5/字节）计分，严格压过单字节代码页字节翻倍误读
                    // （如 CP1253 把 3 字俄文 `Том` 拆成 6 字大写 `ΠΆΠΎΠΌ`，每字节至多 +3）与 GBK (+3/字节)
                    10 * len
                } else {
                    // 2 字母全大写（如 GBK `小小` 撞出的 `СС`）给温和加分，避免误伤双字中文
                    4 * len
                }
            } else {
                -4 * len
            }
        }
        ZipNameCodec::CodePage(cp) if !is_cjk_code_page(cp) => {
            if glued_to_ascii {
                return -6 * len;
            }
            if run.len() == 1 {
                0
            } else if is_case_coherent_bicameral_word(run) {
                let all_upper = run.iter().all(|c| c.is_uppercase());
                // CP1251 把 2 字节 UTF-8 (`0xC2..0xDF` + `0x80..0xBF`) 误读为「俄文大写 + 0x80..0xBF 区的
                // 扩展西里尔（Ђ/Ќ/њ/ў/ѕ/ј 等，落在 U+0410..=U+044F 之外）」，如 `À` -> `ГЂ`、`Ü` -> `Гњ`。
                // 对这类 2 字符组合不给正向词分，防止 CP1251 反向劫持 UTF-8 欧文孤立字母。
                let is_short_cp1251_utf8_pair = run.len() == 2
                    && run.iter().any(|c| {
                        matches!(c, '\u{0400}'..='\u{040F}' | '\u{0450}'..='\u{045F}')
                    });
                if is_short_cp1251_utf8_pair {
                    0
                } else if run.len() == 2 && all_upper {
                    2 * len
                } else {
                    // 单字节代码页每字符仅占 1 字节：+3/字节配系统先验 (+3) 足以压过 GBK (+3/字节)，
                    // 同时绝不会因 2 字节 UTF-8 被拆成 2 个单字节字母而反超 UTF-8 (+5/字节)
                    3 * len
                }
            } else {
                -4 * len
            }
        }
        ZipNameCodec::CodePage(_) => -4 * len,
    }
}

/// 按解码后的 Unicode 文本打分：
/// - 结合书写系统、词内上下文与大小写形态识别真实 UTF-8（含欧文、俄文、希腊、越南、泰、阿、希伯来与 CJK）
/// - 同时精准重罚 GBK 撞 2 字节合法 UTF-8 产生的典型假象（`Ŀ¼`、`ҳ`、`ͼ`、`ѡ`、孤立重音符与私用区）
#[cfg(target_os = "windows")]
fn score_zip_name_text(codec: ZipNameCodec, text: &str) -> i64 {
    let chars: Vec<char> = text.chars().collect();
    let mut score = 0i64;
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];

        // 1. 拉丁词段（ASCII 字母 + 常见欧文/罗马音重音字母）
        if ch.is_ascii_alphabetic() || is_common_european_latin(ch) {
            let mut end = i + 1;
            while end < chars.len()
                && (chars[end].is_ascii_alphabetic() || is_common_european_latin(chars[end]))
            {
                end += 1;
            }
            let run = &chars[i..end];
            let has_ascii = run.iter().any(|c| c.is_ascii_alphabetic());
            for (k, &c) in run.iter().enumerate() {
                if c.is_ascii() {
                    continue;
                }
                score += match codec {
                    ZipNameCodec::Utf8 => {
                        if has_ascii && is_latin_char_coherent_in_word(run, k) {
                            // 合法词内重音（如 Café / Pokémon / Mädchen / Shōnen / Über）：
                            // 权重需压过 GBK (+6+3) 与 CP949 (+7+3) 单字重合分
                            12
                        } else if has_ascii {
                            -4
                        } else if is_utf8_2byte_outside_gb2312(c) {
                            // 落在 GB2312 区间之外（第二字节 <= 0xA0，如 `À`/`à`/`Ü`/`É`），
                            // 在 GBK/Big5/Shift-JIS 中均属生僻或非法，可安全判定为 UTF-8
                            8
                        } else {
                            // 孤立单字重音且落在 GB2312 区（如 GBK「猫」-> `è`），避免误抢单字中文目录
                            -2
                        }
                    }
                    ZipNameCodec::CodePage(cp) if !is_cjk_code_page(cp) => {
                        if has_ascii && is_latin_char_coherent_in_word(run, k) {
                            4
                        } else {
                            -4
                        }
                    }
                    ZipNameCodec::CodePage(_) => -6,
                };
            }
            i = end;
            continue;
        }

        // 2. 现代西里尔 / 现代希腊 / 现代亚美尼亚词段
        if is_modern_cyrillic(ch) || is_modern_greek(ch) || is_modern_armenian(ch) {
            let same_script: fn(char) -> bool = if is_modern_cyrillic(ch) {
                is_modern_cyrillic
            } else if is_modern_greek(ch) {
                is_modern_greek
            } else {
                is_modern_armenian
            };
            let mut end = i + 1;
            while end < chars.len() && same_script(chars[end]) {
                end += 1;
            }
            let glued_left = i > 0 && chars[i - 1].is_ascii_alphabetic();
            let glued_right = end < chars.len() && chars[end].is_ascii_alphabetic();
            score += score_bicameral_word(codec, &chars[i..end], glued_left, glued_right);
            i = end;
            continue;
        }

        // 3. 希伯来 / 阿拉伯词段（无大小写）
        if is_modern_hebrew(ch) || is_modern_arabic(ch) {
            let same_script: fn(char) -> bool = if is_modern_hebrew(ch) {
                is_modern_hebrew
            } else {
                is_modern_arabic
            };
            let mut end = i + 1;
            while end < chars.len() && same_script(chars[end]) {
                end += 1;
            }
            let len = (end - i) as i64;
            let glued_left = i > 0 && chars[i - 1].is_ascii_alphabetic();
            let glued_right = end < chars.len() && chars[end].is_ascii_alphabetic();
            let glued = glued_left || glued_right;
            score += match codec {
                ZipNameCodec::Utf8 => {
                    if glued_left && glued_right {
                        -4 * len
                    } else if len >= 2 {
                        10 * len
                    } else {
                        -2
                    }
                }
                ZipNameCodec::CodePage(cp) if !is_cjk_code_page(cp) => {
                    if glued {
                        -6 * len
                    } else if len >= 2 {
                        3 * len
                    } else {
                        0
                    }
                }
                ZipNameCodec::CodePage(_) => -4 * len,
            };
            i = end;
            continue;
        }

        // 4. 其余单字符打分
        score += match ch {
            // ASCII 可见字符与常见空白：任何编码下都合法，完全中性
            ' '..='~' | '\t' => 0,
            // macOS NFD 分解式拉丁组合变音符（如 `e` + U+0301 = `é`，UTF-8 为 0xCC 0x80..0x8C）
            '\u{0300}'..='\u{030C}'
                if codec == ZipNameCodec::Utf8
                    && i > 0
                    && chars[i - 1].is_ascii_alphabetic() =>
            {
                8
            }
            // 日文假名只可能来自 Shift-JIS 或 UTF-8 的正确解码；
            // GB2312 的 0xA4/0xA5 行也放了假名，所以其它代码页下只给一个可忽略的小分，
            // 否则 Big5 归档里 `A4 A4`(い) 这类全角假名行会把 GBK 顶上去
            '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' => {
                match codec {
                    ZipNameCodec::Utf8 | ZipNameCodec::CodePage(932) => 10,
                    _ => 2,
                }
            }
            // 韩文音节只可能来自 CP949 / GB18030 / UTF-8。常见音节给足分，
            // 冷僻音节（ㄸ/ㅃ/ㅉ 与罕见韵尾组合）反而扣分，避免 GBK 被 CP949 反向劫持；
            // UTF-8 下合法 3 字节韩文几乎不可能由其它遗留编码偶然拼出，额外给足权重避免被 GBK/CP949 抢走
            '\u{AC00}'..='\u{D7A3}' => match codec {
                ZipNameCodec::Utf8 => {
                    if is_common_hangul_syllable(ch) {
                        10
                    } else {
                        6
                    }
                }
                ZipNameCodec::CodePage(949) | ZipNameCodec::CodePage(54936) => {
                    if is_common_hangul_syllable(ch) {
                        7
                    } else {
                        -1
                    }
                }
                _ => -4,
            },
            // 韩文兼容字母 (ㄱ..ㅣ)：韩文文件名的常见成分
            '\u{3130}'..='\u{318F}' => 4,
            // 全角标点与 CJK 符号：中日韩文件名的常见成分
            '\u{3000}'..='\u{303F}' | '\u{FF01}'..='\u{FF60}' => 2,
            // 汉字：遗留代码页之间都能解出汉字，因此保持中性；
            // 但 UTF-8 下每个汉字是严格的 3 字节 `1110xxxx 10xxxxxx 10xxxxxx` 序列，
            // 遗留编码偶然拼出合法 3 字节 UTF-8 汉字的概率极低，必须给予正分，
            // 否则未置 UTF-8 标志的纯中文 UTF-8 归档（如 "图集/001.jpg"、"完结/图精.jpg"）
            // 会因 GBK 字节区间加分（最高可达每 2 汉字 +15 分）反超 0 分的 UTF-8 而变成乱码。
            '\u{4E00}'..='\u{9FFF}' => match codec {
                ZipNameCodec::Utf8 => 10,
                _ => 0,
            },
            // 3 字节 UTF-8 的常见非 CJK 现代文字（越南语扩展拉丁、泰文、天城文/印度系、格鲁吉亚、藏/缅/高棉）：
            // 遗留代码页极难偶然拼出合法 3 字节 UTF-8，在 UTF-8 候选下应给予高权重
            '\u{1E00}'..='\u{1EFF}'
            | '\u{0900}'..='\u{10FF}'
            | '\u{1780}'..='\u{17FF}' => match codec {
                ZipNameCodec::Utf8 => 10,
                _ => -6,
            },
            // 3/4 字节 UTF-8 常用排版标点、符号与 Emoji（— … № ★ ♪ ♥ 箭头等）
            '\u{2010}'..='\u{2027}'
            | '\u{2030}'..='\u{203F}'
            | '\u{2100}'..='\u{27BF}'
            | '\u{1F300}'..='\u{1FAFF}' => match codec {
                ZipNameCodec::Utf8 => 6,
                _ => -1,
            },
            // CP949 会在 GBK 字节上解出兼容区汉字，通常是误读
            '\u{F900}'..='\u{FAFF}' => {
                if codec == ZipNameCodec::CodePage(949) {
                    -6
                } else {
                    -2
                }
            }
            // CJK 扩展 A / 私用区 / 替换字符：正常文件名几乎不会出现
            '\u{3400}'..='\u{4DBF}' | '\u{E000}'..='\u{F8FF}' | '\u{FFFD}' => -10,
            // 剩余 2 字节区：Latin-1 符号（GBK `录` -> `¼`）、罕见 Latin Ext-A（GBK `目` -> `Ŀ`）、
            // Latin Ext-B/IPA、古希腊（GBK `图` -> `ͼ`）、古教会斯拉夫与少数民族西里尔扩展（GBK `页` -> `ҳ`）
            '\u{00A0}'..='\u{017F}' => -6,
            '\u{0180}'..='\u{052F}' => -8,
            c if c.is_control() => -20,
            _ => -1,
        };
        i += 1;
    }
    score
}

/// 构造 ZIP 条目名编码探测的候选池。
///
/// 关键点是**与系统区域设置无关地恒定评估全部候选**（UTF-8 / GBK / Shift-JIS / Big5 / CP949 /
/// GB18030，再加上系统代码页）。旧实现分成「主池 → 回退池」两段，只有主池（936/932/ACP）无法
/// 100% 严格解码时才评估 950/949；但 Big5 与 CP949 的字节区间几乎完全落在 GB2312 合法范围内
/// ——例如 Big5 的 `第1話/封面.jpg` 是 `B2 C4 31 B8 DC 2F AB CA AD B1`，GBK 能把它完整解成
/// `材1杠/靘·`——于是 `valid == len` 恒成立，回退分支永远走不到，整包被静默误解成简体乱码。
/// 现在所有候选在同一场评分里按总分量级决胜负，顺序仅用于同分时的确定性取舍。
#[cfg(target_os = "windows")]
fn zip_name_candidate_pool(sys_acp: u32) -> Vec<ZipNameCodec> {
    let mut pool = vec![ZipNameCodec::Utf8];
    // 936 排在 54936 之前：GBK 归档里 936 与 GB18030 的二维区间解码结果完全一致，
    // 让行为稳定的 936 优先胜出；只有出现 4 字节序列、936 严格解码失败时才轮到 GB18030。
    for cp in [936u32, 932, 950, 949, 54936, sys_acp] {
        let cand = ZipNameCodec::CodePage(cp);
        if !pool.contains(&cand) {
            pool.push(cand);
        }
    }
    pool
}

/// 在整包维度联合评估所有候选编码，返回总分最高的那一个。
#[cfg(target_os = "windows")]
fn choose_zip_name_codec(probe: &[&[u8]], sys_acp: u32) -> Option<ZipNameCodec> {
    // 只要有一条严格解码失败就重罚，等价于「优先完整解码」，但不必再单独维护 valid_count 主键
    const DECODE_FAIL_PENALTY: i64 = 1000;

    let mut best: Option<(ZipNameCodec, i64)> = None;
    for codec in zip_name_candidate_pool(sys_acp) {
        // 系统区域设置本身就是一条弱先验：分值持平时让它胜出，但绝不能盖过真正的编码特征
        let mut total = if codec == ZipNameCodec::CodePage(sys_acp) {
            3i64
        } else {
            0i64
        };
        for &raw in probe {
            let decoded = match codec {
                ZipNameCodec::Utf8 => std::str::from_utf8(raw).ok().map(|s| s.to_string()),
                ZipNameCodec::CodePage(cp) => win_encoding::decode_strict(cp, raw),
            };
            match decoded {
                Some(text) => {
                    if let ZipNameCodec::CodePage(cp) = codec {
                        total += score_zip_name_bytes(cp, raw);
                    }
                    total += score_zip_name_text(codec, &text);
                }
                None => total -= DECODE_FAIL_PENALTY,
            }
        }
        if best.is_none_or(|(_, best_score)| total > best_score) {
            best = Some((codec, total));
        }
    }
    best.map(|(codec, _)| codec)
}

fn decode_zip_entry_names_with_acp(entries: &[RawZipName<'_>], _sys_acp: u32) -> Vec<String> {
    // 只有「没有 UTF-8 声明 + 含非 ASCII 字节」的条目才需要参与代码页探测：
    // 纯 ASCII 在所有候选编码下逐字节一致；带 UTF-8 标志（或已生效 Info-ZIP 0x7075 扩展字段）
    // 的条目由归档自己声明过编码，必须直接信任，否则 GBK 字节恰好是合法 UTF-8 时会被误判。
    let probe: Vec<&[u8]> = entries
        .iter()
        .filter(|e| !e.utf8 && !e.raw.is_ascii())
        .map(|e| e.raw)
        .collect();

    #[cfg(target_os = "windows")]
    let chosen: Option<ZipNameCodec> = if probe.is_empty() {
        None
    } else {
        choose_zip_name_codec(&probe, _sys_acp)
    };

    entries
        .iter()
        .map(|entry| {
            if entry.utf8 || entry.raw.is_ascii() {
                // ASCII 在 UTF-8 与全部候选代码页下逐字节一致，直接按 UTF-8 处理即可
                return String::from_utf8_lossy(entry.raw).into_owned();
            }
            #[cfg(target_os = "windows")]
            if let Some(codec) = chosen {
                match codec {
                    ZipNameCodec::Utf8 => {
                        if let Ok(s) = std::str::from_utf8(entry.raw) {
                            return s.to_string();
                        }
                    }
                    ZipNameCodec::CodePage(cp) => {
                        if let Some(decoded) = win_encoding::decode_strict(cp, entry.raw) {
                            return decoded;
                        }
                    }
                }
            }
            String::from_utf8_lossy(entry.raw).into_owned()
        })
        .collect()
}

/// 批量按归档级别自动探测并解码 ZIP/CBZ 原始条目名：
/// - 已置 UTF-8 标志（含 Info-ZIP Unicode 扩展字段）与纯 ASCII 的条目直接按 UTF-8 保留；
/// - 其余条目（中文 GBK/GB18030、日文 Shift-JIS、繁体 Big5、韩文 CP949）在整包维度联合评估最佳编码，
///   彻底避免乱码与 Shift-JIS/GBK 第二字节 `0x5C` (`\`) 导致的路径错切。
pub fn decode_zip_entry_names(entries: &[RawZipName<'_>]) -> Vec<String> {
    #[cfg(target_os = "windows")]
    let sys_acp = win_encoding::system_acp();
    #[cfg(not(target_os = "windows"))]
    let sys_acp = 0u32;

    decode_zip_entry_names_with_acp(entries, sys_acp)
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

    /// 构造「没有 UTF-8 标志」的条目列表（绝大多数国产/日产打包工具的默认行为）
    #[cfg(target_os = "windows")]
    fn unflagged<'a>(raw: &[&'a [u8]]) -> Vec<RawZipName<'a>> {
        raw.iter().map(|&r| RawZipName::new(r, false)).collect()
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_decode_zip_entry_names_gbk_and_shift_jis_with_0x5c() {
        // 1. GBK 编码中文归档："第1话/封面.jpg", "第1话/001.jpg"
        let gbk_1: &[u8] = &[
            0xB5, 0xDA, b'1', 0xBB, 0xB0, b'/', 0xB7, 0xE2, 0xC3, 0xE6, b'.', b'j', b'p', b'g',
        ];
        let gbk_2: &[u8] = &[0xB5, 0xDA, b'1', 0xBB, 0xB0, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'];
        let gbk_decoded = decode_zip_entry_names(&unflagged(&[gbk_1, gbk_2]));
        assert_eq!(gbk_decoded, vec!["第1话/封面.jpg", "第1话/001.jpg"]);

        // 2. Shift-JIS 编码日文归档（特意包含第二字节为 0x5C '\' 的「表」[0x95, 0x5C] 与「ソ」[0x83, 0x5C]）
        // "第1話/表紙.jpg", "第1話/ソラ.jpg"
        let sjis_1: &[u8] = &[
            0x91, 0xE6, b'1', 0x98, 0x62, b'/', 0x95, 0x5C, 0x8E, 0x86, b'.', b'j', b'p', b'g',
        ];
        let sjis_2: &[u8] = &[
            0x91, 0xE6, b'1', 0x98, 0x62, b'/', 0x83, 0x5C, 0x83, 0x89, b'.', b'j', b'p', b'g',
        ];
        let sjis_decoded = decode_zip_entry_names(&unflagged(&[sjis_1, sjis_2]));
        assert_eq!(sjis_decoded, vec!["第1話/表紙.jpg", "第1話/ソラ.jpg"]);
        assert!(
            !sjis_decoded[0].contains('\\'),
            "Shift-JIS 中含 0x5C 的「表」不应被错切为反斜杠"
        );

        // 3. 标准 UTF-8 归档保持不变
        let utf8_name = "第01話/表紙.png".as_bytes();
        assert_eq!(
            decode_zip_entry_names(&unflagged(&[utf8_name])),
            vec!["第01話/表紙.png"]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_utf8_flag_prevents_gbk_bytes_being_misread_as_utf8() {
        // GBK 的「目录」= C4 BF C2 BC，本身就是合法的 2 字节 UTF-8（会被读成 "Ŀ¼"）；
        // 「1页」= 31 D2 B3，同样合法（会被读成 "1ҳ"）。
        // 旧实现用 `std::str::from_utf8(raw).is_ok()` 做短路，于是同一个目录被拆成
        // `Ŀ¼/001.jpg` 与 `目录/封面.jpg` 两个互相独立的目录，相对排序键错乱、成品 PDF 页序跟着乱。
        let mixed_dir_flat: &[u8] = b"\\\xC4\xBF\xC2\xBC/001.jpg";
        let mixed_dir_gbk: &[u8] = &[
            0xC4, 0xBF, 0xC2, 0xBC, b'/', 0xB7, 0xE2, 0xC3, 0xE6, b'.', b'j', b'p', b'g',
        ];
        // 先确认第一条测试数据确实踩中了「GBK 字节恰好是合法 UTF-8」的陷阱：
        // 整条名字（含「目录」）都是合法 UTF-8，会被旧实现整条读成 "Ŀ¼/001.jpg"
        assert_eq!(
            std::str::from_utf8(&mixed_dir_flat[1..]).unwrap(),
            "Ŀ¼/001.jpg"
        );

        // 没有 UTF-8 标志（两个条目都没有）→ 整包按 GBK 统一解码，目录不再被撕裂
        let decoded = decode_zip_entry_names(&unflagged(&[mixed_dir_flat, mixed_dir_gbk]));
        assert_eq!(decoded, vec!["\\目录/001.jpg", "目录/封面.jpg"]);

        // 一旦归档显式声明了 UTF-8 标志，就必须无条件信任它，不能再按本地代码页重新探测
        let flagged = vec![
            RawZipName::new(b"/\xC4\xBF\xC2\xBC/001.jpg", true),
            RawZipName::new("目录/封面.jpg".as_bytes(), true),
        ];
        assert_eq!(
            decode_zip_entry_names(&flagged),
            vec!["/Ŀ¼/001.jpg", "目录/封面.jpg"]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_utf8_archive_without_flag_still_decodes_as_utf8() {
        // 部分工具写出的归档内容是 UTF-8，却没有置 UTF-8 标志位。
        // 候选池里 UTF-8 必须自己参与竞争，否则整包会被按 GBK 解成乱码。
        let names: [&[u8]; 3] = [
            "第01話/表紙.png".as_bytes(),
            "第01話/001.png".as_bytes(),
            "第02話/目次.png".as_bytes(),
        ];
        assert_eq!(
            decode_zip_entry_names(&unflagged(&names)),
            vec!["第01話/表紙.png", "第01話/001.png", "第02話/目次.png"]
        );

        // 连续偶数个纯汉字的未标记 UTF-8 名称（3 字节 UTF-8 按 2 字节重切后恰好全部落在合法 GBK 区间，
        // 如 "图集" 的 `BE E9`、"图精" 的 `BE E7`/`B2 BE` 均落入 GB2312 一级字区）：
        // 任何系统代码页下都必须识别为 UTF-8，而不能被 GBK 字节加分反杀成乱码。
        let hanzi_pairs: [&[u8]; 3] = [
            "图集/001.jpg".as_bytes(),
            "完结/图精.jpg".as_bytes(),
            "特装/附页.jpg".as_bytes(),
        ];
        for acp in [936u32, 932, 949, 950, 1252] {
            for &single in &hanzi_pairs {
                assert_eq!(
                    decode_zip_entry_names_with_acp(&unflagged(&[single]), acp),
                    vec![std::str::from_utf8(single).unwrap()],
                    "ACP={acp} 时纯中文未标记 UTF-8 归档不应被 GBK 劫持"
                );
            }
            assert_eq!(
                decode_zip_entry_names_with_acp(&unflagged(&hanzi_pairs), acp),
                vec!["图集/001.jpg", "完结/图精.jpg", "特装/附页.jpg"],
                "ACP={acp}"
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_big5_archive_detected_on_simplified_chinese_locale() {
        // 繁体 Big5 路径：字节区间几乎完全落在 GB2312 合法区内，
        // GBK 能把整包「完整解码成功」成乱码，所以旧的「主池 100% 解码就短路」逻辑永远救不回来。
        let big5_1: &[u8] = &[
            0xB2, 0xC4, b'1', 0xB8, 0xDC, b'/', 0xAB, 0xCA, 0xAD, 0xB1, b'.', b'j', b'p', b'g',
        ];
        let big5_2: &[u8] = &[
            0xBA, 0xA9, 0xB5, 0x65, 0xB2, 0xC4, b'0', b'1', 0xA8, 0xF7, b'/', 0xA5, 0xD8, 0xBF,
            0xFD, b'.', b'j', b'p', b'g',
        ];
        let big5_3: &[u8] = &[
            0xC1, 0x63, 0xC5, 0xE9, 0xA4, 0xA4, 0xA4, 0xE5, b'/', 0xB4, 0xFA, 0xB8, 0xD5, b'.',
            b'j', b'p', b'g',
        ];

        for acp in [936u32, 932, 950, 949, 1252] {
            let decoded = decode_zip_entry_names_with_acp(&unflagged(&[big5_1]), acp);
            assert_eq!(decoded, vec!["第1話/封面.jpg"], "ACP={acp}");
        }
        let decoded = decode_zip_entry_names_with_acp(&unflagged(&[big5_2, big5_3]), 936);
        assert_eq!(decoded, vec!["漫畫第01卷/目錄.jpg", "繁體中文/測試.jpg"]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_cp949_archive_detected_on_simplified_chinese_locale() {
        // 韩文 CP949 路径同样能被 GBK 完整解码（"力1拳/钎瘤.jpg"），
        // 旧实现里 949 只在「主池失败」的回退分支才参战，于是简体中文系统上永远识别不出来。
        let kr_1: &[u8] = &[
            0xC1, 0xA6, 0x31, 0xC8, 0xAD, 0x2F, 0xC7, 0xA5, 0xC1, 0xF6, 0x2E, 0x6A, 0x70, 0x67,
        ];
        let kr_2: &[u8] = &[
            0xBD, 0xD6, 0xB5, 0xD5, 0xC0, 0xCC, 0x2F, 0x31, 0xC8, 0xAD, 0x2E, 0x6A, 0x70, 0x67,
        ];
        for acp in [936u32, 932, 949, 950, 1252] {
            let decoded = decode_zip_entry_names_with_acp(&unflagged(&[kr_1, kr_2]), acp);
            assert_eq!(
                decoded,
                vec!["제1화/표지.jpg", "쌍둥이/1화.jpg"],
                "ACP={acp} 时韩文 CP949 归档不应被 GBK 抢走"
            );
        }
    }

    #[test]
    fn test_derive_folder_sequence_name_handles_drive_roots_and_relative_paths() {
        use std::path::Path;
        assert_eq!(derive_folder_sequence_name(Path::new(r"E:\Manga\第01卷")), "第01卷");
        assert_eq!(derive_folder_sequence_name(Path::new(r"D:\")), "D盘");
        assert_eq!(derive_folder_sequence_name(Path::new("/")), "根目录");
        assert_eq!(derive_folder_sequence_name(Path::new(r"E:\Manga\sub\..")), "Manga");
        assert_eq!(derive_folder_sequence_name(Path::new(r"E:\Manga\..")), "E盘");
        assert!(!derive_folder_sequence_name(Path::new(r"\\server\share")).ends_with('盘'));

        // 相对路径（命令行 `MangaPDF.exe .`）：旧实现会落进盘符分支生成 ".盘"
        let cwd_name = std::env::current_dir()
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .filter(|n| !n.trim().is_empty());
        if let Some(expected) = cwd_name {
            assert_eq!(derive_folder_sequence_name(Path::new(".")), expected);
        }
        assert_ne!(derive_folder_sequence_name(Path::new(".")), ".盘");
    }

    #[test]
    fn test_parent_dir_non_empty_ignores_empty_relative_parent() {
        use std::path::{Path, PathBuf};
        // `Path::new("vol1.cbz").parent()` 是 Some("")，绝不能拿去当输出目录
        assert_eq!(Path::new("vol1.cbz").parent(), Some(Path::new("")));
        assert_eq!(parent_dir_non_empty(Path::new("vol1.cbz")), None);
        assert_eq!(
            parent_dir_non_empty(Path::new(r"E:\Manga\vol1.cbz")),
            Some(PathBuf::from(r"E:\Manga"))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_zip_name_candidate_pool_always_covers_every_supported_code_page() {
        // 任何系统区域设置下，GBK / Shift-JIS / Big5 / CP949 / GB18030 / UTF-8 都必须参与同一场角逐，
        // 否则「Big5 或 CP949 字节恰好能被 GBK 完整解码」的归档会永远走不到扩展分支。
        for acp in [936u32, 932, 949, 950, 1252, 437] {
            let pool = zip_name_candidate_pool(acp);
            assert!(pool.contains(&ZipNameCodec::Utf8), "ACP={acp} 时 UTF-8 必须参战");
            for cp in [936u32, 932, 950, 949, 54936] {
                assert!(
                    pool.contains(&ZipNameCodec::CodePage(cp)),
                    "ACP={acp} 时 CP{cp} 必须参战"
                );
            }
            assert!(
                pool.contains(&ZipNameCodec::CodePage(acp)),
                "ACP={acp} 自身也要参与角逐"
            );
            let mut deduped = pool.clone();
            deduped.sort_by_key(|c| format!("{:?}", c));
            deduped.dedup();
            assert_eq!(
                deduped.len(),
                pool.len(),
                "ACP={acp} 时候选池不应有重复项"
            );
        }
        // 简体系统下池的顺序：UTF-8 → 936 → 932 → 950 → 949 → 54936（仅用于同分取舍）
        assert_eq!(
            zip_name_candidate_pool(936),
            vec![
                ZipNameCodec::Utf8,
                ZipNameCodec::CodePage(936),
                ZipNameCodec::CodePage(932),
                ZipNameCodec::CodePage(950),
                ZipNameCodec::CodePage(949),
                ZipNameCodec::CodePage(54936),
            ]
        );
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

    #[cfg(target_os = "windows")]
    #[test]
    fn test_gbk_archive_not_hijacked_by_cp949_on_korean_locale_or_fallback() {
        // "第1话/封面.jpg", "第01卷/彩页/目录.jpg" (GBK 字节恰好全部落在 0xB0..=0xC8 与 949 谚文重合区的前者 + 含 0xC9..=0xF7 的后者)
        let gbk_1: &[u8] = &[
            0xB5, 0xDA, b'1', 0xBB, 0xB0, b'/', 0xB7, 0xE2, 0xC3, 0xE6, b'.', b'j', b'p', b'g',
        ];
        let gbk_2: &[u8] = &[
            0xB5, 0xDA, b'0', b'1', 0xBE, 0xED, b'/', 0xB2, 0xCA, 0xD2, 0xB3, b'/', 0xC4, 0xBF,
            0xC2, 0xBC, b'.', b'j', b'p', b'g',
        ];

        // 1. 无论系统是简体 / 繁体 / 韩文 / 日文 / 西欧，GBK 归档都必须解成简体中文而不是韩文乱码
        for acp in [936u32, 949, 950, 932, 1252] {
            assert_eq!(
                decode_zip_entry_names_with_acp(&unflagged(&[gbk_1]), acp),
                vec!["第1话/封面.jpg"],
                "ACP={acp}"
            );
            assert_eq!(
                decode_zip_entry_names_with_acp(&unflagged(&[gbk_1, gbk_2]), acp),
                vec!["第1话/封面.jpg", "第01卷/彩页/目录.jpg"],
                "ACP={acp}"
            );
        }

        // 2. 归档里混入 1 个所有 CJK 代码页都无法严格解码的条目时，
        //    惩罚是「每条一次」而不是整体丢弃，GBK 仍然不能被 949 抢走
        let broken_entry: &[u8] = b"bad_\xFF.jpg";
        for acp in [936u32, 949, 950, 932] {
            let decoded = decode_zip_entry_names_with_acp(&unflagged(&[gbk_1, broken_entry]), acp);
            assert_eq!(
                decoded[0], "第1话/封面.jpg",
                "ACP={acp} 混入坏条目后不应将 GBK 解成韩文乱码"
            );
        }

        // 3. 真实韩文 CP949 归档在韩文系统 (ACP = 949) 下仍应保持正确韩文解码，不被反向误伤
        let kr_1: &[u8] = &[
            0xC1, 0xA6, 0x31, 0xC8, 0xAD, 0x2F, 0xC7, 0xA5, 0xC1, 0xF6, 0x2E, 0x6A, 0x70, 0x67,
        ];
        let kr_2: &[u8] = &[
            0xBD, 0xD6, 0xB5, 0xD5, 0xC0, 0xCC, 0x2F, 0x31, 0xC8, 0xAD, 0x2E, 0x6A, 0x70, 0x67,
        ];
        let kr_3: &[u8] = &[
            0xC6, 0xAF, 0xC1, 0xFD, 0x2F, 0xBB, 0xE7, 0xB0, 0xC7, 0x2F, 0xB3, 0xA1, 0xC6, 0xC7,
            0x2E, 0x6A, 0x70, 0x67,
        ];
        assert_eq!(
            decode_zip_entry_names_with_acp(&unflagged(&[kr_1, kr_2, kr_3]), 949),
            vec!["제1화/표지.jpg", "쌍둥이/1화.jpg", "특집/사건/끝판.jpg"]
        );
        let kr_fallback =
            decode_zip_entry_names_with_acp(&unflagged(&[kr_1, kr_2, kr_3, broken_entry]), 949);
        assert_eq!(
            &kr_fallback[..3],
            ["제1화/표지.jpg", "쌍둥이/1화.jpg", "특집/사건/끝판.jpg"]
        );
    }

    #[test]
    fn test_normalize_windows_explorer_path_replaces_forward_slashes() {
        use std::path::Path;
        let norm = normalize_windows_explorer_path(Path::new("E:/Manga/out/vol1.pdf"));
        assert!(!norm.contains('/'));
        assert!(norm.ends_with(r"E:\Manga\out\vol1.pdf"));
    }

    #[test]
    fn test_verbatim_prefix_stripping_handles_unc_and_drive_paths() {
        // 普通扩展路径：只剥 `\\?\`
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\E:\Manga\out\vol1.pdf"),
            r"E:\Manga\out\vol1.pdf"
        );
        // 网络路径：必须还原成 `\\server\share\...`，
        // 裸剥 `\\?\` 会得到 explorer 打不开的 `UNC\server\share\a.pdf`
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\UNC\server\share\a.pdf"),
            r"\\server\share\a.pdf"
        );
        // 普通 UNC 与非扩展路径保持原样，只把正斜杠换反斜杠
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\server\share/a.pdf"),
            r"\\server\share\a.pdf"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"E:\Manga\vol1.pdf"),
            r"E:\Manga\vol1.pdf"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_unflagged_utf8_european_cyrillic_and_other_non_cjk_not_hijacked_by_gbk() {
        // 1. 欧文 / 日文罗马音 / 越南语 / macOS NFD 分解形式（未置 UTF-8 标志）
        //    注意 `é` (C3 A9) 在 GBK 中是 GB2312 一级字「茅」、在 CP949 中是常用韩文「챙」，是最容易被劫持的用例
        let european_cases = [
            "Café/001.jpg",
            "Pokémon/001.jpg",
            "CAFÉ/001.jpg",
            "PokéBall/001.jpg",
            "Épisode 01/Crème brûlée.jpg",
            "Mädchen/Über/Straße 01.jpg",
            "Español/São Paulo/Français.jpg",
            "Shōnen Jump/001.jpg",
            "Łódź/Český/Türkçe.jpg",
            "Tiếng Việt/Tập 01.jpg",
            "Cafe\u{0301}/001.jpg",
            "À la mode/001.jpg",
            "à la mode/001.jpg",
            "Ü 01/001.jpg",
        ];

        // 2. 俄文 / 乌克兰文 / 希腊文 / 泰文 / 阿拉伯文 / 希伯来文 / 排版符号与 Emoji
        //    注意 `Том` (D0 A2 D0 BE D0 BC) 全部 3 个字符都恰好落在 GB2312 一级字区，最考验词段判别
        let other_non_cjk_cases = [
            "Глава 1/Обложка.jpg",
            "Том 01/001.jpg",
            "ТОМ 01/001.jpg",
            "Том 01/Часть 1/Страница 01.jpg",
            "Українська/Розділ 1.jpg",
            "Κεφάλαιο 1/001.jpg",
            "ตอนที่ 1/001.jpg",
            "الفصل 1/001.jpg",
            "פרק 1/001.jpg",
            "Vol01 — Special ★/001.jpg",
        ];

        for acp in [936u32, 932, 949, 950, 1252, 1251, 1250, 1253, 1254, 1255, 1256, 874] {
            for &name in european_cases.iter().chain(other_non_cjk_cases.iter()) {
                let decoded = decode_zip_entry_names_with_acp(&unflagged(&[name.as_bytes()]), acp);
                assert_eq!(
                    decoded,
                    vec![name],
                    "ACP={acp} 时未置 UTF-8 标志的 \"{name}\" 不应被遗留代码页劫持"
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_gbk_valid_utf8_traps_still_decode_as_gbk() {
        // 即使归档里只有单条恰好是合法 2 字节 UTF-8 的 GBK 文件名，也绝不能退化成乱码
        let gbk_traps: [(&[u8], &str); 7] = [
            // "目录/001.jpg" -> UTF-8 "Ŀ¼/001.jpg"
            (&[0xC4, 0xBF, 0xC2, 0xBC, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'], "目录/001.jpg"),
            // "1页.jpg" -> UTF-8 "1ҳ.jpg"
            (&[b'1', 0xD2, 0xB3, b'.', b'j', b'p', b'g'], "1页.jpg"),
            // "图/001.jpg" -> UTF-8 "ͼ/001.jpg"
            (&[0xCD, 0xBC, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'], "图/001.jpg"),
            // "猫/001.jpg" -> UTF-8 "è/001.jpg"（孤立重音，无 ASCII 词上下文）
            (&[0xC3, 0xA8, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'], "猫/001.jpg"),
            // "CG猫/001.jpg" -> UTF-8 "CGè/001.jpg"（大写缩写后接小写重音，大小写不自然）
            (&[b'C', b'G', 0xC3, 0xA8, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'], "CG猫/001.jpg"),
            // "小/001.jpg" -> UTF-8 "С/001.jpg"（单字孤立西里尔）
            (&[0xD0, 0xA1, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'], "小/001.jpg"),
            // "懈小/001.jpg" -> UTF-8 "иС/001.jpg"（小写接大写，大小写不自然）
            (&[0xD0, 0xB8, 0xD0, 0xA1, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'], "懈小/001.jpg"),
        ];

        for (raw, expected) in gbk_traps {
            assert!(
                std::str::from_utf8(raw).is_ok(),
                "测试用例 {expected} 必须是合法 UTF-8 字节序列以验证反劫持能力"
            );
            // 简体中文系统下必须解出原 GBK 中文
            assert_eq!(
                decode_zip_entry_names_with_acp(&unflagged(&[raw]), 936),
                vec![expected],
                "ACP=936 时 GBK 陷阱 \"{expected}\" 不应被 UTF-8 劫持"
            );
            // 无论在任何区域设置下，都绝不能被误判成 UTF-8 乱码（如 Ŀ¼ / ҳ / ͼ / è / иС）
            let utf8_mojibake = std::str::from_utf8(raw).unwrap();
            for acp in [936u32, 932, 949, 950, 1252, 1251] {
                let decoded = decode_zip_entry_names_with_acp(&unflagged(&[raw]), acp);
                assert_ne!(
                    decoded[0], utf8_mojibake,
                    "ACP={acp} 时 GBK 陷阱 \"{expected}\" 不应解成 UTF-8 乱码 \"{utf8_mojibake}\""
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_genuine_cp1252_cp1251_and_gb18030_4byte_archives() {
        // 1. 真正的 CP1252 单字节西欧归档在西欧系统 (ACP=1252) 下应正确解码，不被 Big5/GBK 劫持
        //    "Über/Mädchen.jpg" (DC 62 65 72 / 4D E4 64 63 68 65 6E) 与 "Café/Pokémon.jpg"
        let cp1252_1: &[u8] = &[0xDC, b'b', b'e', b'r', b'/', b'M', 0xE4, b'd', b'c', b'h', b'e', b'n', b'.', b'j', b'p', b'g'];
        let cp1252_2: &[u8] = &[b'C', b'a', b'f', 0xE9, b'/', b'P', b'o', b'k', 0xE9, b'm', b'o', b'n', b'.', b'j', b'p', b'g'];
        assert_eq!(
            decode_zip_entry_names_with_acp(&unflagged(&[cp1252_1]), 1252),
            vec!["Über/Mädchen.jpg"]
        );
        assert_eq!(
            decode_zip_entry_names_with_acp(&unflagged(&[cp1252_2]), 1252),
            vec!["Café/Pokémon.jpg"]
        );

        // 2. 真正的 CP1251 单字节俄文归档在俄文系统 (ACP=1251) 下应正确解码
        //    "Тома/001.jpg" (D2 EE EC E0，4 字节恰好也是合法 GBK) 与 "Глава 1/001.jpg"
        let cp1251_toma: &[u8] = &[0xD2, 0xEE, 0xEC, 0xE0, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'];
        let cp1251_glava: &[u8] = &[0xC3, 0xEB, 0xE0, 0xE2, 0xE0, b' ', b'1', b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'];
        assert_eq!(
            decode_zip_entry_names_with_acp(&unflagged(&[cp1251_toma]), 1251),
            vec!["Тома/001.jpg"]
        );
        assert_eq!(
            decode_zip_entry_names_with_acp(&unflagged(&[cp1251_glava]), 1251),
            vec!["Глава 1/001.jpg"]
        );

        // 3. GB18030 四字节字符（如 `𠂇` = 95 32 90 31）应正确按 CP54936 解码
        let gb18030_4b: &[u8] = &[0x95, 0x32, 0x90, 0x31, b'/', b'0', b'0', b'1', b'.', b'j', b'p', b'g'];
        assert_eq!(
            decode_zip_entry_names_with_acp(&unflagged(&[gb18030_4b]), 936),
            vec!["𠂇/001.jpg"]
        );
    }
}

