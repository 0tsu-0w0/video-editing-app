# Forge

自然言語の指示から、AviUtl2 の標準エフェクトとして使えるスクリプト(`.anm2`)を作るツール。
AI の API は叩かない。依頼文をコピーして手持ちの AI(Claude など)に貼り、返答を取り込む。

## 状態(2026-11-13)

| 項目 | 状態 |
|---|---|
| 仕様の検証・`.anm2` の生成・生成物の再検証(`forge-core`) | 実装済み。テスト 13 件 |
| HLSL コンパイル・試走(`forge-gpu`、D3D11) | 実装済み。テスト 7 件 |
| コマンド(`forge-cli`) | 実装済み。通しで動作確認済み |
| **AviUtl2 実機での確認** | **未実施**(→ `docs/phase0-checklist.md`) |
| GUI、Anthropic API 連携 | 未着手 |

## 使い方

`bin/forge.exe` がビルド済みの実行ファイル。

```
forge prompt "VHS風のノイズと色ズレ" -o req.txt     依頼文を作る → AI に貼る
forge import reply.txt                              AI の返答を取り込む → out/ に .anm2 など
forge install "out/VHSノイズ.anm2"                  AviUtl2 のスクリプトフォルダへコピー
```

- `import` が失敗すると `out/repair_prompt.txt` ができる。それを AI に貼り、返答をもう一度 `import` する。
- `forge revise <.au2fx> "もっと強く"` で修正依頼文を作れる。
- `forge render <.au2fx> --input 画像.png --set id=値 -o out.png` で手元の画像に試せる。
- `examples/*.json` に手書きのサンプルが 4 本ある(`forge build examples/vhs.json`)。

## 仕組み

1. AI は **HLSL の関数とパラメータ定義だけ**を JSON(EffectSpec)で返す。
2. Forge が定数バッファ・テクスチャの宣言を前に付け、Lua を固定テンプレートから作る。
   AI が書いた文字列は、設定項目のヘッダ行と、長いコメント内の HLSL にしか入らない。
3. 検証 → 静的検査 → コンパイル → 試走(NaN・全面同色・効果なし・全透明を検出)。
4. `.anm2` を書き出し、別実装の検証器で読み直して、テンプレート由来の行だけであることを確かめる。

## ビルド

```
cargo test --workspace
cargo build --release -p forge-cli
```

Rust(MSVC)と Windows 10/11 が必要。テストは WARP(ソフトウェア描画)で動くので GPU は不要。

**注意:** フォルダのパスが長いとリンクに失敗する(LNK1104)。`C:\dev\forge` のような短い場所に置くか、
環境変数 `CARGO_TARGET_DIR` に短いパスを指定する。

## 構成

```
crates/forge-core   spec(検証) / hlsl(前置コード・静的検査) / emit(.anm2 生成)
                    verify(生成物の再検証) / prompt(依頼文・JSON 取り出し) / au2fx(保存形式)
crates/forge-gpu    d3d(D3D11) / analyze(試走の判定) / lib(描画・試走)
crates/forge-cli    forge コマンド
examples/           サンプルの EffectSpec
docs/               host-notes(AviUtl2 の確定仕様) / phase0-checklist(実機確認の手順)
```
