//! studio-core: 動画編集ソフトの中核(UI・GPU・OS に依存しない)。
//!
//! - `time`: 時間(flick 単位の整数)とフレームレート
//! - `project`: 素材・トラック・クリップ・キーフレーム
//! - `command`: 全ての編集操作(画面・AI・MCP 共通)
//! - `editor`: 実行・トランザクション・試し実行・Undo/Redo
//! - `eval`: ある時刻に描く映像レイヤーと鳴らす音声

pub mod command;
pub mod editor;
pub mod eval;
pub mod project;
pub mod time;

pub use command::{Command, EditError};
pub use editor::{Editor, Origin, Outcome};
pub use project::{Asset, AssetKind, Clip, Id, Project, Track, TrackKind};
pub use time::{Rate, Time};

#[cfg(test)]
mod tests;
