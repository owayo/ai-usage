//! 行の並び順(--sort)。CLI とレンダラー間で共有する。

use clap::ValueEnum;

/// `--sort` の値。`render` 層もこの enum を参照してレンダラー間で挙動を揃える。
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[value(rename_all = "kebab-case")]
pub enum SortKey {
    /// 既存挙動 — table/json はフェッチ順、statusline は provider.rank() → profile 名。
    #[default]
    Provider,
    /// 長期枠の使用率が高い順(降順)。リミットに近いアカウントを上に表示する。
    WeeklyUsage,
    /// 長期枠のリセット時刻が近い順(昇順)。リセット待ちが短いアカウントを上に。
    WeeklyReset,
}
