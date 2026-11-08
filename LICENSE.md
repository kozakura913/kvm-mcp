# License / ライセンス

**EN:** This project (source code, original documentation, scripts and configuration
in this repository, *except* anything under `third-party/`) is dedicated / offered to
the public under a dual license. You may choose either one:

- **BSD Zero Clause License (0BSD)** — full text: [`LICENSE-0BSD`](LICENSE-0BSD)
- **Creative Commons CC0 1.0 Universal** — full text: [`LICENSE-CC0-1.0`](LICENSE-CC0-1.0)

```
SPDX-License-Identifier: 0BSD OR CC0-1.0
```

**日本語:** 当リポジトリのプロジェクト（ソースコード、当リポジトリ独自の文書・
スクリプト・設定ファイル。ただし `third-party/` 以下を除く）は、下記の
**0BSD と CC0 1.0 のデュアルライセンス**です。どちらか好きな方を選んで利用できます。

## 除外される資料 (Exceptions)

- `third-party/wch/` — WCH（南京沁恒微電子 / WCH.cn）の公式資料、およびそれを
  原資料とする日本語訳・翻案（`ch9329-spec*.md`）。**原資料の著作権は WCH に
  あり、上記デュアルライセンスの適用対象ではありません。** 実装の参照目的で
  同梱しているだけです。各ファイルのライセンス詳細は
  [`third-party/wch/README.md`](third-party/wch/README.md) を参照してください。
  再配布が不適切と判断した場合は、このディレクトリごと削除してください
  （元プロトコル PDF の公開 URL は同 README に記載）。
- `server/` のビルドで使われる Rust クレート（tiny_http, serde_json, libc, image など）
  は、それぞれ上流のライセンス (MIT / Apache-2.0 等) のままです。
