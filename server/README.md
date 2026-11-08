# usb-mcp — UVC/HID Streamable HTTP MCPサーバー

Raspberry Pi（pi-mcp.lan）に直結した下記デバイスを、**MCP（Streamable HTTP）**
経由で取得・操作するサーバーです。

- **UVC キャプチャ**（MS2109、`/dev/video0`）= ターゲットPCのHDMI映像
- **HID 注入**（CH340E UART `ttyUSB0` → CH9329 → ターゲットPCのUSB）

Rust 製・依存最小（tiny_http / serde_json / libc / image[png のみ・純Rust]、
base64 は自作）。

## 使い方（配置済み）

- Pi 上で systemd サービス `usb-mcp` として常駐している（`http://pi-mcp.lan:8765/mcp`）
- 管理: `ssh -F ../.ssh/config pi-mcp.lan 'sudo systemctl {status|restart|stop} usb-mcp'`
- ログ: `... 'journalctl -u usb-mcp -n 50'`

MCP クライアント設定（OpenCode の例、`opencode.json`）:

```json
{
  "mcp": {
    "usb": {
      "type": "remote",
      "url": "http://pi-mcp.lan:8765/mcp"
    }
  }
}
```

> **ccserver サンドボックスからの注意**: この VM からの通常の HTTP はサンドボックスの
> 透過プロキシに interception され port 8765 では 502 になる。その場合は
> SSH ポートフォワードで回避する（通常の LAN 利用では素直に直結できる）:
> ```bash
> ssh -F ../.ssh/config -f -N -L 18766:127.0.0.1:8765 pi-mcp.lan
> # → url: http://127.0.0.1:18766/mcp
> ```

## ツール一覧（11個）

| ツール | 内容 |
|---|---|
| `capture_info` | UVC デバイス一覧（driver=uvcvideo のみ。bcm2835-codec/isp 等のプラットフォームノードは除外）・対応フォーマット（text） |
| `capture_frame` | 1 フレーム取得。YUYV 生取り込み → 可逆 PNG（flate2 deflate 圧縮、`image` クレート / 純Rust）。信号変化を確実に追随するため、画素差分・文字可読性の検証に向く（数百KB）。**MJPEG(JPEG) モードは 2026-11-08 廃止**（先頭ヘッダ破損、フォーマット/信号切替直後に MS2109 の古いノーシグナルカラーバーが残る）。warmup ストリーム破棄付き。`skip=既定2` で先頭フレーム不安定対策（サイズ検証 + 自動リトライ）。**モード追加（2026-11-08）**: `mode=image`（既定、PNG のみ）/ `mode=diff`（前回キャプチャとの変化画素数 % と bbox をテキストのみで返す。画像を送らない低コストな「画面動いた？」確認。ベースラインはデバイスごとにメモリ内、再起動・解像度変更でリセット）/ `mode=both`（統計 + PNG）。`region={x,y,w,h}` で矩形クロップ（境界超過はクランプ、原点外はエラー。diff の計測範囲にもなる）。`diff_threshold`（既定12、0=ビット完全一致。ライブ信号のノイズ対策）。diff は RGB 変換前の YUYV バイト比較（閾値超で Y=個別画素、U/V=ペア2画素に反映） |
| `hid_status` | GET_INFO。firmware と **usb_status（0x01=ターゲットが HID 認識済み）** を確認。セッション開始時に推奨 |
| `hid_key_press` | 修飾キー + 同時押し（最大6キー）→ hold → 全解放 |
| `hid_type_text` | 文字列入力（US 配列 ASCII、shift 自動） |
| `hid_release_keys` | 全キー解放（固着キー救出用、安全） |
| `hid_media_key` | メディアキー（CMD 0x03）。**ACPI（power/sleep）は意図的に非実装** |
| `hid_mouse_move` | 相対移動（推奨）。±127 で自動分割 |
| `hid_mouse_click` | 左/右/中クリック（相対フレーム経由） |
| `hid_mouse_scroll` | ホイール |
| `hid_mouse_abs_move` | 絶対配置。**ターゲットでズレが実測されているため警告付き。基本は相対移動 + キャプチャで合わせる** |

安全設計：

- CH9329 の**隠しコマンド 0x07/0x0D/0x0E および 0x0F を超える CMD はサーバー側で拒否**
  （V1.4 ファームウェアで USB 側が消える事故の教訓、`third-party/wch/ch9329-spec.md` §5 警告）。拒否は単体テスト済み
- ボタン/ホイールは絶対フレームでは送らない（本基板では Linux に到達しないため、実測（§13））
- 全フレーム SUM は自動計算。SUM 不一致はターゲット注入ゼロで E4 として返る
- ACK 待ち 500ms。ゴミバイトからの再同期、V1.4 の 5 バイト短縮 ACK both 対応

## ビルド（このマシンでクロスコンパイル）

Pi Zero 上ではビルドできない（173MB RAM では不可能）。**ローカル Docker で
armv6l 向けにクロスビルドし**、バイナリだけを送る。

```bash
./build.sh test    # ネイティブ（x86_64）単体テスト
./build.sh build   # arm-unknown-linux-musleabi 完全静的リンク → target/arm-unknown-linux-musleabi/release/usb-mcp
```

仕組み：

- `vendor/arm-linux-musleabi-cross.tgz` = musl.cc の ARMv5 ベースライン（musleabi）
  クロスツールチェーン。展開済み。入手：
  `curl -o vendor/arm-linux-musleabi-cross.tgz https://musl.cc/arm-linux-musleabi-cross.tgz`
  （展開は `tar --no-same-owner` が必要。fuse-overlayfs では chown が失敗する）
- `arm-unknown-linux-musleabi` は tier-3 なので **stable + `RUSTC_BOOTSTRAP=1`
  + `-Zbuild-std`** で std も再構築。このモードでは crt オブジェクト/libgcc/unwind
  が sysroot に同梱されないため、build.sh がツールチェーンから
  `<rustc sysroot>/lib/rustlib/<target>/lib[/self-contained]` へコピーする
- `.cargo/config.toml` にリンカと `+crt-static` を設定済み

生成物は**完全静的リンク（musl）**なので、ターゲットの glibc バージョンに依存せず
armv6l（Pi Zero）/ armv7 以降の any ARM で動く。670KB、strip 済み。

ccserver サンドボックスでビルドする際は、プロキシ CA を渡すために：

```bash
BUILD_DOCKER_ARGS='-e CARGO_HTTP_CAINFO=/run/ccserver-ca.pem -e SSL_CERT_FILE=/run/ccserver-ca.pem \
  --mount type=bind,src=/etc/ssl/certs/ca-certificates.crt,dst=/run/ccserver-ca.pem,readonly' ./build.sh build
```

（これはサンドボックス固有の回避策。通常のネットワーク環境では `./build.sh build` だけでよい）

## デプロイ

```bash
./deploy.sh          # scp → systemd インストール → enable --now
```

`deploy/usb-mcp.service` を Pi の `/etc/systemd/system/` に導入する。
設定は unit ファイルの Environment ディレクティブで変更できる（既定: port 8765、/dev/video0、/dev/ttyUSB0、9600 baud）。

## プロトコル実装メモ

- Streamable HTTP: `POST /mcp` に JSON-RPC 2.0 リクエストを送信。**レスポンスは application/json
  の単一応答**（SSE 配信はしないので `GET /mcp` は spec 通り 405 を返す）
- `initialize` で `Mcp-Session-Id` を発行（以後の要求は必須、不明はエラー）。
  `DELETE /mcp` でセッション破棄。CORS はすべて許可
- protocolVersion は 2025-06-18 / 2025-03-26 / 2024-11-05 / 2024-10-07 に対応

## ディレクトリ

```
server/
  src/main.rs      HTTP + JSON-RPC + ツールディスパッチ
  src/ch9329.rs    UART ドライバ（フレーム生成 / ACK 解析 / 危険 CMD 拒否）+ 単体テスト
  src/capture.rs   v4l2-ctl ラッパー（YUYV 生フレームのみ、サイズ検証 + skip 増加リトライ）
  src/png.rs       YUYV→RGB + `image`（png のみ）で圧縮 PNG エンコード + 単体テスト
  src/keys.rs      HID usage テーブル / 文字エンコード + 単体テスト
  src/b64.rs       base64
  build.sh         ローカル Docker クロスビルド
  deploy.sh        Pi へ scp + systemd 導入
  deploy/usb-mcp.service
  .cargo/config.toml
  vendor/          musl クロスツールチェーン
```

## 関連ドキュメント

- `../third-party/wch/ch9329-spec.md` — プロトコル仕様（実機検証済みの訂正・警告含む。WCH 公式資料の派生なので当プロジェクトのライセンス対象外）
- `../memo/20261107-target-hid-injection.md` — 実働記録（注入の判定基準など）
- `AGENTS.md` — ハードウェア構成と環境の癖

## ライセンス

コード・スクリプト・当リポジトリ独自文書は **0BSD / CC0-1.0 デュアルライセンス**
（`LICENSE.md`、SPDX: `0BSD OR CC0-1.0`）。`../third-party/wch/` 配下（WCH 公式マニュアル
PDF とその日本語訳・翻案）は WCH に著作権があり、上記ライセンスの適用対象外。
