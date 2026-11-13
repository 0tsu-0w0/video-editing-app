# AviUtl2 の確定仕様(host-notes)

出典: この PC に入っている AviUtl ExEdit2 **version 2.1.16** 同梱の `lua.txt` / `aviutl2.txt` と、
同梱スクリプト(`script.anm2`)、導入済みスクリプト(`OpenGlow.anm2` など)。
「文書で確認」は公式文書に書いてあること、「実機未確認」は Forge の生成物でまだ試していないこと。

## Phase 0 項目の結果

| ID | 内容 | 結果 |
|---|---|---|
| P0-1 | スクリプトの置き場・再読み込み | **文書で確認。** `ProgramData\aviutl2\Script\`(と1つ下のフォルダ)。「キャッシュを破棄」でスクリプトとシェーダは再読み込みされるが、**設定項目の変更は反映されない**(新規追加・項目変更は再起動が必要と思われる。実機未確認) |
| P0-2 | 設定項目の種類 | **文書で確認。** `--track@` `--check@` `--color@` `--select@` `--file@` `--font@` `--figure@` `--text@` `--string@` `--value@` `--group:` `--separator:` `--hide@` など。`--select@変数名:項目名=既定値,選択肢=値,…` |
| P0-3 | 定数バッファ | **文書で確認。** 「定数バッファ(b0)に float の配列として設定」。HLSL 側は通常のパッキング規則なので、Forge は**スカラー float だけ**を並べる(ずれない)。ハーネスで並びを検証済み |
| P0-4 | tempbuffer と複数パス | **文書で確認。** `target` と `resource` に `"object"` `"tempbuffer"` `"framebuffer"` `"cache:名前"` を指定可。resource には `"random"`(256x256、R32F)も可。target と同じバッファを読む場合は複製される。tempbuffer の初期化に `obj.copybuffer("tempbuffer","object")` を使う点は**実機未確認** |
| P0-5 | アルファ・座標・サンプラ | **文書で確認。** 内部形式は RGBA16bitFloat の**乗算済みα**。入力は `float4 pos : SV_Position` と任意で `float2 uv : TEXCOORD`(描画範囲が 0〜1)。サンプラは引数で `clip/clamp/loop/mirror/dot` を指定し、**既定は未設定**なので必ず指定する |
| P0-6 | パラメータ数の上限 | 文書に記載なし。OpenGlow は 12 項目で動作。Forge は 24 個までに制限(実機未確認) |
| P0-7 | コンパイル設定・エラー時の挙動 | 文書に記載なし。**実機未確認** |
| P0-8 | 時間などの値 | **文書で確認。** `obj.time` `obj.totaltime` `obj.frame` `obj.totalframe` `obj.w` `obj.h`。乱数は `obj.rand(st,ed,seed,frame)` |
| P0-9 | Lua の版 | **文書で確認。** 既定は LuaJIT 2.1(`--script:lua` で Lua 5.1.4)。`os` `debug` `ffi.C` は外されている。スクリプト制御(テキスト内)は `table` `string` `math` のみ |
| P0-10 | コンピュートシェーダ | **文書で確認。** `--[[computeshader@名前:` と `obj.computeshader(...)`。Forge は使わない |
| P0-11 | 配布条件 | 未確認 |

## 呼び出しの書式(重要)

```
obj.pixelshader(name, target, {resource,...} [, {constant,...}, blend, sampler])
```

- **第2引数が出力先、第3引数が入力**(コミュニティ資料の説明とは逆だった)。
- `blend`: `"copy"`(既定) / `"mask"`(α値のみ乗算) / `"draw"`(アルファブレンド) / `"add"`
- 登録名がエントリーポイント名になる。`'登録名@スクリプト名'` で他スクリプトのシェーダも呼べる。

## その他

- 文字コードは UTF-8(BOM なし)、改行は CRLF(同梱スクリプトに合わせた)。
- ファイル名の先頭が `@` だと「複数スクリプトをまとめたファイル」になり、行頭の `@名前` が区切りになる。
  Forge はファイル名に `@` を許さず、シェーダの各行を必ずタブで始める。
- `--filter` を付けるとフィルタオブジェクトでも使える。条件は「オブジェクトのサイズを変えない」こと。
  Forge の生成物はサイズを変えないので `--filter` を付けている(実機未確認)。
- `--label:` には独自の名前を付けられる(導入済みスクリプトが `Basic_S\加工` などを使用)。Forge は `Forge`。
- `--information:` は自由な文字列。

## 要件定義書(v0.3)との差分

| 項目 | v0.3 | 実装(確定) |
|---|---|---|
| cbuffer の宣言 | AI が規約どおりに書く | **Forge が生成して前置する**(並びの不一致が原理的に起きない) |
| テクスチャ・サンプラ | AI が宣言 | Forge が `tex0`〜`tex3` と `smp` を宣言 |
| Lua の変数名 | id をそのまま | `p_` を付ける(`obj` や `RGB` など既存の名前と衝突しない) |
| HLSL 内の `]]` | 拒否 | `] ]` に書き換え(配列の `a[b[0]]` は正当なため)。コメントは全て除去 |
| `select` | 未確認のため保留 | 対応 |
| 複数パス | 最大4 | 最大6、`blend` と `sampler` をパスごとに指定可 |
| Lua パーサでの再解析 | `full_moon` 等 | 厳格な行文法の検証器を自作(依存を減らし、許す形を最小にした) |
| `.au2fx` の中身 | spec + シェーダ別ファイル + サムネイル | `manifest.json` と `spec.json` のみ(ハッシュ検証つき) |
| パスの長さ | — | 長いパスでリンクが失敗する。短い場所に置く |
