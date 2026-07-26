//! テキスト正規化（PLAN §7-3）。
//!
//! ここは Discord にも ENGINE にも依存しない純粋関数にする。仕様がケースの集合なので、
//! テストを厚く書いて挙動を固定する。Discord 側の解決結果（表示名など）は
//! [`Names`] に詰めて渡してもらう。

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// 解決できなかったときの読み。
const UNKNOWN_USER: &str = "誰か";
const UNKNOWN_ROLE: &str = "ロール";
const UNKNOWN_CHANNEL: &str = "どこか";

const CODE_BLOCK_READING: &str = "コード省略";
const URL_READING: &str = "URL省略";
const ATTACHMENT_READING: &str = "ファイル";
const TRUNCATED_SUFFIX: &str = "以下略";

/// 同じ文字がこれ以上続いたら圧縮する。
const MAX_REPEAT: usize = 3;

static CODE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```.*?```").expect("正規表現が不正"));
static INLINE_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`]*)`").expect("正規表現が不正"));
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://\S+").expect("正規表現が不正"));
static CUSTOM_EMOJI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<a?:([^:>]+):\d+>").expect("正規表現が不正"));
static USER_MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<@!?(\d+)>").expect("正規表現が不正"));
static ROLE_MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<@&(\d+)>").expect("正規表現が不正"));
static CHANNEL_MENTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<#(\d+)>").expect("正規表現が不正"));
/// 笑いの `w` / `ｗ` の連続。単独の英単語を壊さないよう 2 文字以上に限る。
static LAUGH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[wｗ]{2,}").expect("正規表現が不正"));

/// Discord 側で解決した名前。ID が引けなければ既定の読みにフォールバックする。
#[derive(Debug, Default, Clone)]
pub struct Names {
    pub users: HashMap<u64, String>,
    pub roles: HashMap<u64, String>,
    pub channels: HashMap<u64, String>,
}

/// 正規化の入力条件。
#[derive(Debug, Clone)]
pub struct Options<'a> {
    pub max_length: usize,
    pub names: &'a Names,
    /// サーバー辞書（表記, 読み）。長い表記から順に適用する。
    pub dictionary: &'a [(String, String)],
    /// 添付ファイルの数。本文が空でも添付があれば「ファイル」と読む。
    pub attachments: usize,
}

impl Default for Options<'_> {
    fn default() -> Self {
        static EMPTY_NAMES: LazyLock<Names> = LazyLock::new(Names::default);
        Self {
            max_length: 100,
            names: &EMPTY_NAMES,
            dictionary: &[],
            attachments: 0,
        }
    }
}

/// 読み上げ用テキストへ変換する。読むものが無ければ `None`。
pub fn normalize(input: &str, options: &Options<'_>) -> Option<String> {
    // コードブロックが先。中に URL やメンションが入っていても丸ごと潰す。
    let text = CODE_BLOCK.replace_all(input, CODE_BLOCK_READING);
    let text = INLINE_CODE.replace_all(&text, "$1");
    let text = URL.replace_all(&text, URL_READING);
    let text = CUSTOM_EMOJI.replace_all(&text, "$1");

    let text = USER_MENTION.replace_all(&text, |caps: &regex::Captures<'_>| {
        lookup(&options.names.users, &caps[1], UNKNOWN_USER)
    });
    let text = ROLE_MENTION.replace_all(&text, |caps: &regex::Captures<'_>| {
        lookup(&options.names.roles, &caps[1], UNKNOWN_ROLE)
    });
    let text = CHANNEL_MENTION.replace_all(&text, |caps: &regex::Captures<'_>| {
        format!(
            "#{}",
            lookup(&options.names.channels, &caps[1], UNKNOWN_CHANNEL)
        )
    });

    let text = apply_dictionary(&text, options.dictionary);
    let text = LAUGH.replace_all(&text, "わら").into_owned();
    let text = collapse_newlines(&text);
    let text = compress_repeats(&text);
    // 前後の空白と、改行由来で余った読点を落とす。これをしないと
    // 改行だけのメッセージが「、」として読み上げられてしまう。
    let text = text
        .trim_matches(|ch: char| ch == '、' || ch.is_whitespace())
        .to_owned();

    if text.is_empty() {
        // 本文が無くても添付があれば、その旨だけ読む。
        return (options.attachments > 0).then(|| ATTACHMENT_READING.to_owned());
    }

    Some(truncate(&text, options.max_length))
}

fn lookup(map: &HashMap<u64, String>, raw_id: &str, fallback: &str) -> String {
    raw_id
        .parse::<u64>()
        .ok()
        .and_then(|id| map.get(&id))
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

/// 辞書は長い表記から適用する。短い表記が先に当たって長い表記を壊さないため。
fn apply_dictionary(input: &str, dictionary: &[(String, String)]) -> String {
    let mut entries: Vec<&(String, String)> = dictionary.iter().collect();
    entries.sort_by_key(|(surface, _)| std::cmp::Reverse(surface.chars().count()));

    let mut text = input.to_owned();
    for (surface, reading) in entries {
        if !surface.is_empty() {
            text = text.replace(surface.as_str(), reading);
        }
    }
    text
}

/// 改行は「、」にする。連続していても 1 つにまとめる。
fn collapse_newlines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_break = false;
    for ch in input.chars() {
        if ch == '\n' || ch == '\r' {
            if !in_break {
                out.push('、');
                in_break = true;
            }
        } else {
            in_break = false;
            out.push(ch);
        }
    }
    out
}

/// 同じ文字の連続を [`MAX_REPEAT`] 文字までに詰める。
fn compress_repeats(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut previous = None;
    let mut run = 0usize;
    for ch in input.chars() {
        if Some(ch) == previous {
            run += 1;
        } else {
            previous = Some(ch);
            run = 1;
        }
        if run <= MAX_REPEAT {
            out.push(ch);
        }
    }
    out
}

/// 文字数上限。超えたら末尾に「以下略」を付ける（PLAN §4）。
fn truncate(input: &str, max_length: usize) -> String {
    if input.chars().count() <= max_length {
        return input.to_owned();
    }
    let head: String = input.chars().take(max_length).collect();
    format!("{head}{TRUNCATED_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            users: HashMap::from([(1, "いわ".to_owned())]),
            roles: HashMap::from([(2, "運営".to_owned())]),
            channels: HashMap::from([(3, "雑談".to_owned())]),
        }
    }

    fn normalize_with(input: &str, names: &Names) -> Option<String> {
        normalize(
            input,
            &Options {
                names,
                ..Options::default()
            },
        )
    }

    fn plain(input: &str) -> Option<String> {
        normalize(input, &Options::default())
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(plain("こんにちは"), Some("こんにちは".to_owned()));
    }

    #[test]
    fn urls_are_replaced() {
        assert_eq!(
            plain("これ見て https://example.com/foo?a=1 すごい"),
            Some("これ見て URL省略 すごい".to_owned())
        );
        assert_eq!(plain("http://a.example"), Some("URL省略".to_owned()));
    }

    #[test]
    fn custom_emoji_reads_its_name() {
        assert_eq!(plain("<:kusa:123456789>"), Some("kusa".to_owned()));
        // アニメーション絵文字も同じ。
        assert_eq!(plain("<a:party:987654321>"), Some("party".to_owned()));
    }

    #[test]
    fn mentions_are_resolved() {
        let names = names();
        assert_eq!(
            normalize_with("<@1> おはよう", &names),
            Some("いわ おはよう".to_owned())
        );
        // 旧形式のニックネームメンション。
        assert_eq!(normalize_with("<@!1>", &names), Some("いわ".to_owned()));
        assert_eq!(normalize_with("<@&2>", &names), Some("運営".to_owned()));
        assert_eq!(normalize_with("<#3>", &names), Some("#雑談".to_owned()));
    }

    #[test]
    fn unknown_mentions_fall_back() {
        let names = names();
        assert_eq!(normalize_with("<@999>", &names), Some("誰か".to_owned()));
        assert_eq!(normalize_with("<@&999>", &names), Some("ロール".to_owned()));
        assert_eq!(normalize_with("<#999>", &names), Some("#どこか".to_owned()));
    }

    #[test]
    fn code_blocks_are_omitted_including_their_contents() {
        assert_eq!(
            plain("見て\n```rust\nlet x = 1; // https://example.com\n```\nここ"),
            Some("見て、コード省略、ここ".to_owned())
        );
    }

    #[test]
    fn inline_code_keeps_its_content() {
        assert_eq!(
            plain("`cargo test` を実行"),
            Some("cargo test を実行".to_owned())
        );
    }

    #[test]
    fn newlines_become_punctuation_and_collapse() {
        assert_eq!(plain("あ\nい"), Some("あ、い".to_owned()));
        assert_eq!(plain("あ\n\n\n\nい"), Some("あ、い".to_owned()));
        // 先頭と末尾の改行は落とす。「、」だけ読まれても意味が無い。
        assert_eq!(plain("\nあ\n"), Some("あ".to_owned()));
    }

    #[test]
    fn laughter_is_read_as_word() {
        assert_eq!(plain("それなwwwww"), Some("それなわら".to_owned()));
        assert_eq!(plain("それなｗｗ"), Some("それなわら".to_owned()));
        // 1 文字だけの w は英単語の一部かもしれないので触らない。
        assert_eq!(plain("w"), Some("w".to_owned()));
    }

    #[test]
    fn repeated_characters_are_compressed() {
        assert_eq!(plain("あーーーーーーー"), Some("あーーー".to_owned()));
        assert_eq!(plain("!!!!!!!!"), Some("!!!".to_owned()));
        // 上限以下はそのまま。
        assert_eq!(plain("あーー"), Some("あーー".to_owned()));
    }

    #[test]
    fn long_text_is_truncated_with_suffix() {
        // 同一文字を並べると圧縮が先に効いてしまうので、繰り返しでない文字列を使う。
        let input = "あいうえお".repeat(30);
        let result = plain(&input).expect("読むものがある");
        assert_eq!(result.chars().count(), 100 + "以下略".chars().count());
        assert!(result.ends_with("以下略"));
    }

    #[test]
    fn text_at_the_limit_is_not_truncated() {
        let input = "あいうえお".repeat(20);
        assert_eq!(plain(&input), Some(input));
    }

    #[test]
    fn max_length_is_configurable() {
        let result = normalize(
            "あいうえお",
            &Options {
                max_length: 3,
                ..Options::default()
            },
        );
        assert_eq!(result, Some("あいう以下略".to_owned()));
    }

    #[test]
    fn empty_after_normalization_is_skipped() {
        assert_eq!(plain(""), None);
        assert_eq!(plain("   \n  "), None);
        // URL だけの投稿は「URL省略」が残るので読む。
        assert_eq!(plain("https://example.com"), Some("URL省略".to_owned()));
    }

    #[test]
    fn attachment_only_message_is_announced() {
        let result = normalize(
            "",
            &Options {
                attachments: 1,
                ..Options::default()
            },
        );
        assert_eq!(result, Some("ファイル".to_owned()));
    }

    #[test]
    fn attachment_with_text_reads_the_text() {
        let result = normalize(
            "これ",
            &Options {
                attachments: 2,
                ..Options::default()
            },
        );
        assert_eq!(result, Some("これ".to_owned()));
    }

    #[test]
    fn dictionary_replaces_surface_with_reading() {
        let dictionary = [("VOICEVOX".to_owned(), "ボイスボックス".to_owned())];
        let result = normalize(
            "VOICEVOX を使う",
            &Options {
                dictionary: &dictionary,
                ..Options::default()
            },
        );
        assert_eq!(result, Some("ボイスボックス を使う".to_owned()));
    }

    #[test]
    fn longer_dictionary_entries_win() {
        // 短い表記が先に当たると「東京都」が「トウキョウと」になってしまう。
        let dictionary = [
            ("東京".to_owned(), "トウキョウ".to_owned()),
            ("東京都".to_owned(), "トウキョウト".to_owned()),
        ];
        let result = normalize(
            "東京都に住む",
            &Options {
                dictionary: &dictionary,
                ..Options::default()
            },
        );
        assert_eq!(result, Some("トウキョウトに住む".to_owned()));
    }

    #[test]
    fn dictionary_applies_after_mentions() {
        let names = Names {
            users: HashMap::from([(1, "いわ".to_owned())]),
            ..Names::default()
        };
        let dictionary = [("いわ".to_owned(), "イワ".to_owned())];
        let result = normalize(
            "<@1> さん",
            &Options {
                names: &names,
                dictionary: &dictionary,
                ..Options::default()
            },
        );
        assert_eq!(result, Some("イワ さん".to_owned()));
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // マルチバイトでも文字数で数える。バイト数で切ると panic する。
        let input = "🎉🎈🎊".repeat(50);
        let result = plain(&input).expect("読むものがある");
        assert!(result.ends_with("以下略"));
        assert_eq!(result.chars().count(), 100 + "以下略".chars().count());
    }
}
