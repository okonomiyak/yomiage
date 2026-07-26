//! exVOICE（収録済み音声素材）の再生。
//!
//! 冥鳴ひまりを選んでいるユーザーの発言が素材の文章と一致したら、合成せずに
//! その wav をそのまま鳴らす。
//!
//! 対応表は同梱の CSV ではなく**実ファイルの走査**で作る。CSV には実在しない
//! 行が混ざっていることがあり（配布物で 2 件確認）、鳴らせないキーを持つと
//! 再生時に初めて失敗するため。ファイル名は `<番号>_<文章>.wav` で、
//! 文章の部分は CSV の「ファイル名」列と同じ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::voicevox::StyleId;

/// この素材集を使う話者。冥鳴ひまり（ノーマル）。
pub const STYLE: StyleId = StyleId(14);

/// 走査するディレクトリの深さの上限。素材集は 2〜3 階層。
const MAX_DEPTH: usize = 4;

#[derive(Default)]
pub struct Library {
    /// 文章 → wav のパス。
    entries: HashMap<String, PathBuf>,
}

impl Library {
    /// ディレクトリが無ければ空のライブラリを返す（機能が無効なだけ）。
    pub fn load(dir: &Path) -> Self {
        let mut entries = HashMap::new();
        if dir.is_dir() {
            collect(dir, 0, &mut entries);
        }
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 読み上げ文が素材と一致すればそのパス。前後の空白だけ無視する。
    pub fn find(&self, text: &str) -> Option<&Path> {
        self.entries.get(text.trim()).map(PathBuf::as_path)
    }
}

fn collect(dir: &Path, depth: usize, entries: &mut HashMap<String, PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        tracing::warn!(dir = %dir.display(), "failed to read exvoice directory");
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, depth + 1, entries);
            continue;
        }
        if path
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("wav"))
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        // 先頭の「番号_」を落とす。無い場合はそのまま。
        let key = stem
            .split_once('_')
            .filter(|(head, _)| !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()))
            .map_or(stem, |(_, rest)| rest);

        // 同じ文章のファイルが複数あることがある。先に見つけたものを使う。
        entries.entry(key.to_owned()).or_insert(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_is_not_an_error() {
        let library = Library::load(Path::new("この道は存在しない"));
        assert!(library.is_empty());
        assert_eq!(library.find("ハロー"), None);
    }

    #[test]
    fn keys_drop_the_leading_number() {
        let dir = std::env::temp_dir().join("yomiage-exvoice-test");
        let category = dir.join("実況");
        std::fs::create_dir_all(&category).expect("テスト用ディレクトリを作れない");
        std::fs::write(category.join("001_ハロー.wav"), b"RIFF").expect("書き込めない");
        std::fs::write(category.join("読み上げます.wav"), b"RIFF").expect("書き込めない");
        // wav 以外は拾わない。
        std::fs::write(category.join("002_説明.txt"), b"x").expect("書き込めない");

        let library = Library::load(&dir);

        assert_eq!(library.len(), 2);
        assert!(library.find("ハロー").is_some());
        // 番号が無いファイルもそのまま使える。
        assert!(library.find("読み上げます").is_some());
        // 前後の空白は無視する。
        assert!(library.find("  ハロー  ").is_some());
        assert_eq!(library.find("説明"), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
