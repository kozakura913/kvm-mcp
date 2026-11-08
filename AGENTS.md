# AGENTS.md

## ライセンス方針（2026-11-08 確定）

- プロジェクト本体（コード、AGENTS.md、スクリプト・設定）は **0BSD / CC0-1.0 のデュアルライセンス**（`LICENSE.md`、SPDX: `0BSD OR CC0-1.0`）。
- WCH 公式資料およびその日本語訳・翻案は **`third-party/wch/` に保管。当プロジェクトのライセンス対象外**。これをプロジェクト成果物として扱わないこと。
- プロジェクト側のファイル（AGENTS.md、コードコメント）に WCH 公式文書の文章を翻訳含めそのまま流用しない。WCH 由来仕様の参照は `third-party/wch/` への参照で済ませる。

## pi-mcp.lan への SSH 接続メモ

### 接続先

- ホスト: `pi-mcp.lan` = `10.0.0.79`（Raspberry Pi Zero 系 / armv6l、Raspbian 13 trixie、hostname `raspberrypi`）
- ユーザー: `pi` のみ（root / debian / admin は認証不可）
- 認証: 公開鍵のみ。パスワード認証はサーバー側で無効化済み。
- `pi` はパスワードレス sudo 可能。
- ping (ICMP) は通らない。ポート 22 は開放済み。

### 鍵・設定ファイル（このリポジトリ内）

- `.ssh/id_ed25519` / `.ssh/id_ed25519.pub` — 秘密鍵（mode 600 を維持）
- `.ssh/config` — Host ブロック（User=pi, IdentityFile, known_hosts 指定）
- `.ssh/known_hosts` — `pi-mcp.lan` と `10.0.0.79` の両エントリ登録済み

### 接続コマンド

```bash
ssh -F /home/debian/projects/usb-mcp/.ssh/config pi-mcp.lan
```

非対話シェル（エージェントの shell tool など）ではこれを使うこと。

### 注意事項（環境の癖）

- **素の `ssh` は使えない**: `/etc/ssh/ssh_config` の Include 先 `20-systemd-ssh-proxy.conf` の owner が `nobody` で致命的エラーになる。`/etc` は読み取り専用なので直せない。`-F <config>` でグローバル設定自体を無視させるのが唯一の回避策。
  - `-F /dev/null` でもエラーは消えるが、その場合は鍵・known_hosts・ユーザーを全て `-o` で明示する必要がある。
- `ssh-keyscan` はこの環境では失敗する（接続を切られる）。ホスト鍵の登録は `ssh -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=<file>` の接続で書き込むこと。
- known_hosts の IP エントリは default port 22 なら `[10.0.0.79]` ではなく素の `10.0.0.79` 表記でないと一致しない（不一致 → tty がない環境で確認プロンプトが出せず "Host key verification failed" になる）。
- scp 等にも同じ `-F` を付ける: `scp -F /home/debian/projects/usb-mcp/.ssh/config <src> pi-mcp.lan:<dst>`
- HOME が session によって `/root` か `/home/debian` か揺れる。`~` に依存せず絶対パスで指定すること。

## プロジェクト構成と運用概要

- `server/` — Rust 製 MCP Streamable HTTP サーバー。UVC キャプチャ（MS2109）と CH9329 HID 注入をツール化。
- サーバーは Pi（`pi-mcp.lan`）上で systemd サービス `usb-mcp` として port 8765 で常駐運用。
- CH9329 基板の UART 側（CH340E）→ Pi の `/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0`、HID 側 → ターゲットPC。
- MS2109 HDMI キャプチャ → `/dev/video0`（1280×720）。
- `capture_frame` の出力は YUYV 生取り込み → 可逆 PNG のみ。デバイス MJPEG を使う JPEG モードは不安定（先頭ヘッダ破損・ノーシグナル残像）のため **2026-11-08 に廃止**。経過・再現手順は `memo/20261107-usb-device-status.md`。
- `capture_frame` にモード追加（2026-11-08）: `mode=diff`（前回キャプチャとの差分統計のみ、画像なし）/ `mode=both`、矩形クロップ `region={x,y,w,h}`、`diff_threshold`（既定12）。diff は YUYV バイト比較（`server/src/diff.rs`）、ベースラインはデバイス単位でメモリ内（サーバー再起動・解像度変更でリセット）。
- ビルドは Pi Zero では不可能なため、VM 上の Docker で armv6l（musleabi 完全静的リンク）へクロスビルド → `server/deploy.sh` でデプロイ。

## 詳細メモへの参照

以下の詳細記録は `memo/` へ移動した。調査・検証時に参照すること。

- USB 機器構成・サーバー運用・キャプチャの癖・CH340 トラブルシュート: `memo/20261107-usb-device-status.md`
- ターゲットPCへの HID 注入記録: `memo/20261107-target-hid-injection.md`
- マウス操作の挙動検証: `memo/20261108-mouse-behavior.md`
- CH9329 プロトコル詳細: `third-party/wch/ch9329-spec.md`
