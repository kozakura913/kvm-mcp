# 備忘録: USB 機器構成・usb-mcp サーバー・キャプチャの癖（2026-11-07）

> AGENTS.md から移動した詳細記録。当日の機器状態、検証ログ、トラブルシュートを時系列で残す。

## USB トポロジー（lsusb -t 実測）

```
Pi Zero (dwc_otg)
└─ Genesys 05e3:0608 ハブ（セルフパワー・ポート電源制御なし）
   ├─ port1 → Terminus 1a40:0101 ハブ
   │          ├─ port1 → CH340E (1a86:7523, bcd 2.64) → ttyUSB0  ※CH9329基板内蔵のUART側
   │          └─ (port3 空き)  ※CH9329 の USB-AプラグはターゲットPC側に接続済み
   ├─ port2 → MacroSilicon MS2109 (534d:2109) HDMIキャプチャ → /dev/video0
   └─ port4 → ASIX AX88772 (0b95:7720) USB-LAN
```

- 相手側ホスト（ターゲットPC）は CH9329 の USB(HID) 側に接続する想定。
- 「カラーバー＝電源ON確認」は誤り。カラーバーは MS2109 のノーシグナル表示。
- 2026-11-07 09:59 時点でターゲットの HDMI はキャプチャに到達確認済み（Linux Mint ライブセッション、1280×720@60Hz）。

## usb-mcp サーバー運用メモ

- Rust 製 Streamable HTTP MCP サーバーを Pi 上の systemd サービス `usb-mcp` として常駐運用（port 8765、`POST /mcp`）。
- ソース/ビルド手順は `server/`（README 付き）。Pi Zero ではビルド不可なため、この VM の Docker で armv6l（musleabi 完全静的リンク）へクロスビルド → scp でデプロイ。
- サンドボックスの癖: VM→Pi の素の HTTP（80 番以外）は透過プロキシに interception され 502 になる。VM から叩くときは `ssh -F <config> -f -N -L 18766:127.0.0.1:8765 pi-mcp.lan` で回避。実際の LAN クライアントからは直結で動く。
- 安全: サーバーは CH9329 の隠し CMD（0x07/0x0D/0x0E/0x0F 超）をコード側で拒否済み（単体テストあり）。ACPI(power/sleep) メディアコマンドも非実装。
- CH340 の再採番対策として、サービスは `SERIAL_DEV=/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0`（安定パス）で運用。
- デプロイは `deploy.sh`。`enable --now` だと再起動競合で古い exe を指すことがあるため `systemctl restart` を推奨。実稼働 exe は `/proc/$(pidof usb-mcp)/exe` の sha256 でローカルと照合すること。

## E2E 実注入検証（2026-11-07 12:44）

- スクロールバックに `hid_type_text "echo mcp-e2e-7421\n"` の入力と出力が文字レベルで一致（サブエージェント独立検証 OVERALL:PASS）。
- 既存履歴（`active-term-ok` / `st-closed-ok`）は無傷。
- `hid_mouse_click`（ターミナル本文）後も青×クラスタ 101 px を維持 = クリック到達。
- マウスホイールフレーム自体は ACK 済みだが、直後のキャプチャ事故で視覚効果は未検証。

## 画像判定の実務教訓

- ターミナルの座標はウィンドウ移動でずれる。再キャプチャのたびに測り直すこと（11:20 測定の y0=155 が同日では y0=210 に移動、×ボタンは (733,224) 周辺）。
- カーソル塗りつぶしの YUYV 単フレーム数値判定は不可（ブリンク消灯相を拾うと誤ゼロ）。
- アクティブ判定は青×クラスタ数（JPEG/YUYV）とサブエージェント視認を併用すること。

## MS2109 ハング事故（2026-11-07 12:47〜）

- E2E テスト中 `uvcvideo: Failed to resubmit video URB (-1)` でストリーム 0 バイト化。
- デバイス単体 unbind/bind、ハブサブツリー(1-1)再バインド、USBDEVFS_RESET、uvcvideo モジュール再ロードのすべて無効。
- CH340 precedent どおり、MS2109 の USB-A プラグの**物理抜去**が必要（セルフパワー非 PPT ハブで VBUS 断できない）。
- 復帰後、無信号時フォーマット一覧は 1080p 列に変わる（EDID 再ネゴ）。720p 要求が通らなくなるため、キャプチャは信号復帰後に行うこと。

## capture_frame の PNG モード / v4l2 の癖

- `capture_frame` の既定は `png`（YUYV 生取り込み → `image` クレートで可逆圧縮。画素差分・文字可読に最適、数百 KB）。`format:"jpeg"` はデバイス MJPEG（小さく速いが癖あり）。
- v4l2-ctl の `--stream-count` は「書き込むフレーム数」。`--stream-skip` はその前に捨てる数。`count=skip+1` だと複数フレームが 1 ファイルに連結される。`--stream-count=1` 固定が正しい。
- MS2109 はフォーマット切替直後のフレームが古いノーシグナルカラーバー／黒／Y 低ノイズで不安定（`skip=2` では足りない）。両モードとも warmup ストリーム（skip+6）を捨ててから実取得するよう修正済み。
- v4l2 SET_FMT は非対応解像度を黙って調整する（例: 640x360 YUYV → 640x480）。`capture.rs` の `negotiate()`/`parse_fmt()` が `--get-fmt-video` で実寸・実 fourcc を読み戻し、実寸でエンコード。fourcc が YUYV でない場合はエラー。
- urllib は `http_proxy` 経由でサンドボックスプロキシを通ると空レスポンスで不安定。MCP の E2E は `http.client`（ダイレクト TCP）+ SSH トンネル（`ssh -N -L 127.0.0.1:18779:127.0.0.1:8765`、nohup で起動し最後 kill）で行うこと。

## CH340 トラブルシューティング

- 直接 Genesys ハブ port1 に繋いだ構成では EP0 が -71/-32 で恒常的に STALL、チップがハングした（`cannot read device status`）。
- 途中に Terminus ハブを 1 段挟むと解決。CH340 は完全安定（stty/idle/送受信でエラーゼロ）。原因はターゲットPC方面の配線を経由したグランドループ／ノイズと推定。
- セルフパワー & 非 PPT ハブなので software reset（authorized 切替／USBDEVFS_RESET）では VBUS が切れず、詰んだ CH340 は物理抜去が必要。
- `usbcore.quirks` 等のカーネル側対策は不要だった（エラー時はまず電気側を疑う）。

## CH9329 状況メモ

- 原因は結線ではなくプロトコル誤り。旧メモの `57 05 ...` フレームは CH9329 の公式フレームではなく、チップが誤フレームを黙って破棄していた。
- 基板は CH340E+CH9329 統合（`CH9329_USB-HID_U1.1 [303CH932902]`）。TTL UART ピンはなく、基板端パッド「5V, D+, D-, G」= CH340E の USB 信号。
- 詳細仕様は `third-party/wch/ch9329-spec.md`（実機 V1.4 で検証済み）。

## ターゲット画面の確認方法（キャプチャ経由）

- sudo は不要（pi は video グループ所属）。
- ノーシグナル時は MS2109 内蔵の 8 色カラーバーが出る（= ターゲットの HDMI が届いていない）。
- 取得できた JPEG は SOI `FF D8` で始まる通常の JPEG。ストリーム先頭フレームは途中で切れてヘッダ無効なデータになることがある（`file` で JPEG と出るものだけ使う）。
- `--stream-skip=2` で先頭フレームを捨てるのが安全。`/tmp` に古い残骸があると上書き失敗することがあるので、別名（`/tmp/f2.jpg` など）へ出すこと。

```bash
v4l2-ctl -d /dev/video0 --set-fmt-video=width=1280,height=720,pixelformat=MJPG \
  --stream-mmap --stream-count=3 --stream-skip=2 --stream-to=/tmp/f2.jpg
# scp/rsync ではなく ssh -F <config> pi-mcp.lan 'cat /tmp/f2.jpg' > frame.jpg で取得
```

- 画像の判定（ウィンドウのアクティブ／前面状態など）はサブエージェントに独立検証させること。自分のセッションの画像読解は誤信しない。
- 実効性のある証明は見た目の変化ではなく機能テスト（注入文字列がターミナルに実行結果として出るか）。詳細は `memo/20261107-target-hid-injection.md`。

## CH9329 テストハーネス（Pi 側・再起動で消えるので再作成可）

CH9329 の HID が Pi 側に挿さっている前提。ターゲットへ移した時は by-id パスを target 側で確認すること。

```bash
#!/bin/bash  # /tmp/inj2.sh
PORT=/dev/ttyUSB0
KB=/dev/input/by-id/usb-WWW.WCH.CN_WCH_UART_TO_KB-MS_V1.4_20193952A17B-event-kbd
stty -F $PORT 9600 raw -echo cs8 -parenb clocal
( timeout 6 cat $KB > /tmp/kb_evt.bin ) &
( timeout 6 cat $PORT > /tmp/ack.bin ) &
sleep 0.5
python3 - <<'PY'
import os, time
fd = os.open("/dev/ttyUSB0", os.O_RDWR)
def send(b): os.write(fd, bytes(b))
def kb(k): send([0x57,0xAB,0x00,0x02,0x08,0x00,0x00,k,0,0,0,0,0,(0x10C+k)&0xff])
def rel(): send([0x57,0xAB,0x00,0x02,0x08,0,0,0,0,0,0,0,0,0x0C])
send([0x57,0xAB,0x00,0x01,0x00,0x03]); time.sleep(0.3)  # GET_INFO
rel(); time.sleep(0.2)
kb(0x04); time.sleep(0.12); rel(); time.sleep(0.3)      # a
kb(0x05); time.sleep(0.12); rel(); time.sleep(0.3)      # b
kb(0x28); time.sleep(0.12); rel()                      # Enter
os.close(fd)
PY
wait
od -An -tx1 /tmp/ack.bin
```

> 注: SUM 式 `(0x10C+k)&0xff` は 2026-11-07 再計算で正しい（公式 'a' 例 SUM=0x10 に一致）。
