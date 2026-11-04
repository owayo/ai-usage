//! 行の並び順(--sort)。CLI とレンダラー間で共有する。

use clap::ValueEnum;

/// `--sort` の値。`render` 層もこの enum を参照してレンダラー間で挙動を揃える。
/// `--sort` を省いたときの値はモードで違う (TUI だけ `WeeklyReset`)。解決は `Cli::sort_key` が持つ。
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[value(rename_all = "kebab-case")]
pub enum SortKey {
    /// 既存挙動 — table/json/TUI はフェッチ順、statusline は provider.rank() → profile 名。
    #[default]
    Provider,
    /// 長期枠の使用率が高い順(降順)。リミットに近いアカウントを上に表示する。
    WeeklyUsage,
    /// 長期枠のリセット時刻が近い順(昇順)。リセット待ちが短いアカウントを上に。
    WeeklyReset,
}

impl SortKey {
    /// `--sort` に渡す値と同じ名前。TUI が今の並び順を示すのに使う。
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::WeeklyUsage => "weekly-usage",
            Self::WeeklyReset => "weekly-reset",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_arg_matches_the_clap_value_names() {
        for key in SortKey::value_variants() {
            let value = key.to_possible_value().expect("どの値も --sort で選べる");
            assert_eq!(key.as_arg(), value.get_name());
        }
    }
}
