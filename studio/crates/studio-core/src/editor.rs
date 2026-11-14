//! 編集の窓口: コマンドの実行、まとめて実行(トランザクション)、試し実行、Undo/Redo、操作履歴。
//!
//! 画面・AI・MCP のどこから来た操作も、ここを通る。

use crate::command::{apply, Command, EditError};
use crate::project::{Id, Project};
use crate::Time;
use serde::{Deserialize, Serialize};

/// 操作の出どころ。履歴の表示で、AI の操作を見分けられるようにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    User,
    Ai,
    Mcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub label: String,
    pub origin: Origin,
    /// 実行したコマンド(ID を記録済み)。Redo で使う
    pub forward: Vec<Command>,
    /// 元に戻すコマンド(この順に実行)
    pub inverse: Vec<Command>,
    pub summary: Vec<String>,
    /// 連続した変更をまとめるための目印(例: "text:12")と、最後に変更した時刻
    #[serde(skip)]
    pub merge: Option<(String, std::time::Instant)>,
}

/// 実行結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// 新しく作られた ID(コマンドの順)
    pub created: Vec<Id>,
    /// 変更の要約(人と AI が読む)
    pub summary: Vec<String>,
}

/// 何番目のコマンドで失敗したか。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{index} 番目のコマンド({command})で失敗: {error}")]
pub struct TransactionError {
    pub index: usize,
    pub command: &'static str,
    pub error: EditError,
}

pub struct Editor {
    pub project: Project,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    /// 保存してから変更があったか
    pub dirty: bool,
    pub max_history: usize,
}

/// 取り消し・やり直し用(記録した操作をそのまま適用する)。
fn run(project: &mut Project, cmds: &[Command]) -> Result<(Vec<Command>, Vec<Command>, Outcome), TransactionError> {
    run_with(project, cmds, false)
}

/// コマンドの対象のクリップ(エラーの表示用)。
fn target_of(c: &Command) -> String {
    serde_json::to_value(c).ok().and_then(|v| v.get("clip").map(|x| x.to_string())).unwrap_or_default()
}

/// リンクしたクリップにも同じ操作をするための、追加のコマンド(c を適用する前の状態で決める)。
fn linked_extras(p: &Project, c: &Command) -> Vec<Command> {
    use Command::*;
    let partner = |id: Id| p.clip(id).and_then(|c| c.link).and_then(|o| p.clip(o)).filter(|o| o.link == Some(id));
    // 時間がそろっているか(端の伸縮・速度などは、そろっているときだけ相手にも効かせる)
    let aligned = |a: &crate::project::Clip, b: &crate::project::Clip| {
        a.start == b.start && a.duration == b.duration && a.source_in == b.source_in && a.speed == b.speed && a.reverse == b.reverse
    };
    match c {
        MoveClip { clip, start, .. } => match (p.clip(*clip), partner(*clip)) {
            (Some(a), Some(b)) => vec![MoveClip { clip: b.id, start: (b.start + (*start - a.start)).max(Time::ZERO), track: None }],
            _ => vec![],
        },
        TrimClip { clip, start, duration, source_in } => match (p.clip(*clip), partner(*clip)) {
            (Some(a), Some(b)) if aligned(a, b) => vec![TrimClip { clip: b.id, start: *start, duration: *duration, source_in: *source_in }],
            _ => vec![],
        },
        SplitClip { clip, at, .. } => match partner(*clip) {
            Some(b) if b.contains(*at) && *at > b.start => vec![SplitClip { clip: b.id, at: *at, new_id: None }],
            _ => vec![],
        },
        RemoveClip { clip } => partner(*clip).map(|b| vec![RemoveClip { clip: b.id }]).unwrap_or_default(),
        // 複製は相手も同じだけずらして複製する(トラックは相手のまま)
        DuplicateClip { clip, start, .. } => match (p.clip(*clip), partner(*clip)) {
            (Some(a), Some(b)) => vec![DuplicateClip { clip: b.id, start: (b.start + (*start - a.start)).max(Time::ZERO), track: None, new_id: None }],
            _ => vec![],
        },
        SetSpeed { clip, speed } => match (p.clip(*clip), partner(*clip)) {
            (Some(a), Some(b)) if aligned(a, b) => vec![SetSpeed { clip: b.id, speed: *speed }],
            _ => vec![],
        },
        SetReverse { clip, reverse } => match (p.clip(*clip), partner(*clip)) {
            (Some(a), Some(b)) if aligned(a, b) => vec![SetReverse { clip: b.id, reverse: *reverse }],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// コマンド列を適用し、失敗したら途中までを巻き戻す。
/// `link` なら、リンクしたクリップにも同じ操作をする(新しい操作のとき。取り消し・やり直しでは、記録した操作をそのまま使う)。
fn run_with(project: &mut Project, cmds: &[Command], link: bool) -> Result<(Vec<Command>, Vec<Command>, Outcome), TransactionError> {
    let mut forward = Vec::new();
    let mut inverses: Vec<Vec<Command>> = Vec::new();
    let mut out = Outcome { created: vec![], summary: vec![] };
    let next_id = project.next_id;
    for (i, c0) in cmds.iter().enumerate() {
        let extras = if link { linked_extras(project, c0) } else { vec![] };
        // 分割・複製で相手の分もできたら、新しいクリップどうしをリンクする
        let split_pair = matches!(c0, Command::SplitClip { .. } | Command::DuplicateClip { .. }) && !extras.is_empty();
        let mut created_here = vec![];
        let mut seq: Vec<Command> = std::iter::once(c0.clone()).chain(extras).collect();
        let mut k = 0;
        while k < seq.len() {
            let c = seq[k].clone();
            k += 1;
            match apply(project, &c) {
                Ok(a) => {
                    forward.push(a.resolved);
                    inverses.push(a.inverse);
                    created_here.extend(a.created.iter().copied());
                    if k == 1 {
                        out.created.extend(a.created);
                        out.summary.push(a.summary);
                    } else if let Some(s) = out.summary.last_mut() {
                        s.push_str(&format!("(リンク: {})", a.summary));
                    }
                    // 分割・複製したときは、新しいクリップどうしもリンクする
                    if split_pair && k == seq.len() && seq.len() == 2 && created_here.len() == 2 {
                        seq.push(Command::SetLink { clip: created_here[0], other: Some(created_here[1]) });
                    }
                }
                Err(error) => {
                for inv in inverses.iter().rev() {
                    for c in inv {
                        apply(project, c).expect("巻き戻しに失敗しました(内部エラー)");
                    }
                }
                    project.next_id = next_id; // 失敗した操作で使いかけた ID も戻す
                    let error = if k > 1 { EditError { message: format!("リンクしたクリップ {}: {}", target_of(&c), error.message), ..error } } else { error };
                    return Err(TransactionError { index: i, command: c0.name(), error });
                }
            }
        }
    }
    let inverse: Vec<Command> = inverses.into_iter().rev().flatten().collect();
    Ok((forward, inverse, out))
}

impl Editor {
    pub fn new(project: Project) -> Self {
        Self { project, undo: vec![], redo: vec![], dirty: false, max_history: 1000 }
    }

    /// 1 つのコマンドを実行する。
    pub fn execute(&mut self, cmd: Command, origin: Origin) -> Result<Outcome, TransactionError> {
        let label = cmd.name().to_string();
        self.transaction(&label, vec![cmd], origin)
    }

    /// 複数のコマンドを、1 回の Undo で戻せる単位として実行する。1 つでも失敗したら何も変えない。
    pub fn transaction(&mut self, label: &str, cmds: Vec<Command>, origin: Origin) -> Result<Outcome, TransactionError> {
        if origin != Origin::User {
            if let Some((i, c)) = cmds.iter().enumerate().find(|(_, c)| c.is_internal()) {
                return Err(TransactionError {
                    index: i,
                    command: c.name(),
                    error: EditError { code: "forbidden", message: "内部用のコマンドは使えません".into() },
                });
            }
        }
        let (forward, inverse, out) = run_with(&mut self.project, &cmds, true)?;
        if !forward.is_empty() {
            self.undo.push(HistoryEntry { label: label.to_string(), origin, forward, inverse, summary: out.summary.clone(), merge: None });
            if self.undo.len() > self.max_history {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.dirty = true;
        }
        Ok(out)
    }

    /// 連続した変更(文字の入力、色の調整など)を、1 回の Undo で戻せる 1 つの操作にまとめて実行する。
    /// 直前の操作が同じ `key` で、`window` 以内に行われていれば、その操作に合流させる。
    pub fn transaction_merged(&mut self, label: &str, cmds: Vec<Command>, origin: Origin, key: &str, window: std::time::Duration) -> Result<Outcome, TransactionError> {
        let now = std::time::Instant::now();
        let can_merge = self.redo.is_empty()
            && self.undo.last().is_some_and(|e| e.origin == origin && e.merge.as_ref().is_some_and(|(k, t)| k == key && now.duration_since(*t) <= window));
        if !can_merge {
            let out = self.transaction(label, cmds, origin)?;
            if let Some(e) = self.undo.last_mut() {
                e.merge = Some((key.to_string(), now));
            }
            return Ok(out);
        }
        let (forward, inverse, out) = run_with(&mut self.project, &cmds, true)?;
        let e = self.undo.last_mut().unwrap();
        e.forward.extend(forward);
        // 新しい変更を先に戻し、その後で以前の変更を戻す
        let mut inv = inverse;
        inv.extend(std::mem::take(&mut e.inverse));
        e.inverse = inv;
        e.summary = out.summary.clone();
        e.merge = Some((key.to_string(), now));
        self.dirty = true;
        Ok(out)
    }

    /// 試し実行: プロジェクトを変えずに、成功するかと変更内容を返す。
    pub fn dry_run(&self, cmds: &[Command]) -> Result<Outcome, TransactionError> {
        let mut p = self.project.clone();
        run_with(&mut p, cmds, true).map(|(_, _, o)| o)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// 取り消す。戻り値は取り消した操作の名前。
    pub fn undo(&mut self) -> Option<String> {
        let e = self.undo.pop()?;
        run(&mut self.project, &e.inverse).expect("Undo に失敗しました(内部エラー)");
        let label = e.label.clone();
        self.redo.push(e);
        self.dirty = true;
        Some(label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let mut e = self.redo.pop()?;
        let (_, inverse, _) = run(&mut self.project, &e.forward).expect("Redo に失敗しました(内部エラー)");
        e.inverse = inverse;
        let label = e.label.clone();
        self.undo.push(e);
        self.dirty = true;
        Some(label)
    }

    /// 履歴(古い順)。
    pub fn history(&self) -> &[HistoryEntry] {
        &self.undo
    }
}
