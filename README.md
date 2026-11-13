# video-editing-app

AviUtl2 のような Windows 向け動画編集ソフト(仮称: Forge Studio)を作るプロジェクト。
2D/3D の配置、点・平面・人物のトラッキング、音声編集までを1本のソフトで行い、
AI(言葉での指示・Claude からの操作)からも人間と同じ操作ができるようにする。

**現在の段階: Phase 0(技術検証)の途中。** 動画編集ソフト本体はまだ無い。

## 中身

| パス | 内容 |
|---|---|
| [video-editor-requirements.md](video-editor-requirements.md) | **動画編集ソフトの要件定義書(本命)** |
| [studio-spikes/](studio-spikes/) | Phase 0 の技術検証プログラムと結果([PHASE0-RESULTS.md](studio-spikes/PHASE0-RESULTS.md)) |
| [forge/](forge/) | 言葉からエフェクトを作るツール Forge。AviUtl2 用 `.anm2` を出力する。本ソフトのエフェクト機能の部品になる |
| [ai-dynamic-effect-forge-requirements-v0.3.md](ai-dynamic-effect-forge-requirements-v0.3.md) | Forge の要件定義書(v0.3) |
| [ai-dynamic-effect-forge-requirements.md](ai-dynamic-effect-forge-requirements.md) | Forge の要件定義書(v0.2、旧版) |

## 技術選定(Phase 0 で確定したもの)

- 言語: Rust
- GPU: Direct3D 11(UI・エフェクト・動画デコードで1つのデバイスを共有)
- UI: egui + egui-directx11
- 動画の読み書き: Media Foundation(Windows 標準)
- 音声: cpal(WASAPI)、再生の時刻は音声側の標本数で決める
- 3D: `gltf` クレート + 自前の D3D11 描画
- 点トラッキング: 自前のピラミッド Lucas-Kanade(OpenCV 不使用)

## ビルド

Windows 10/11、Rust(MSVC)が必要。フォルダのパスが長いとリンクに失敗する(LNK1104)ので、
短い場所に置くか `CARGO_TARGET_DIR` に短いパスを指定する。

```
cd forge && cargo test --workspace
cd studio-spikes && cargo run --release --bin klt_track
```
