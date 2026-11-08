# 備忘録: ターゲットPCへの CH9329 HID注入 + キャプチャ検証（2026-11-07）

目的: Pi (pi-mcp.lan) から CH9329 経由で仮想キーボード/マウスを注入し、HDMIキャプチャ（MS2109）でターゲット画面を確認しながら「アクティブウィンドウをターミナルに切り替える」操作を実行・検証した記録。

## 当日の環境状態（10:00 時点）
- CH9329 基板の **USB-AプラグはターゲットPC側**に接続済み（Pi の `lsusb` には `1a86:e129` は出ず、UART側の CH340 `1a86:7523` だけが残る）
- UART 側は Pi の `/dev/ttyUSB0`（Terminus ハブ経由で安定）
- キャプチャ `/dev/video0`（1280×720@60、MJPG）。ターゲットは Linux Mint ライブセッション（`mint@mint:~`）
- 事前設定: `stty -F /dev/ttyUSB0 9600 raw -echo cs8 -parenb clocal`

## タイムライン
| 時刻 | 出来事 |
|---|---|
| 09:59 | ターゲットのHDMI到達を確認（System Settings → Layout 画面）。`captures/frame-20261107-0956.jpg` |
| 10:0x | GET_INFO → `57 ab 00 81 08 34 01 ...`（st=0x01=ターゲットがHID認識済み）を確認 |
| 10:0x | 絶対マウス(CMD 0x04)+相対ボタン(CMD 0x05)でターミナル本文/タスクバーをクリック → ACKは全て成功するが**ポインタが狙い座標に配置されない**。本文クリックで前面raiseのみ発生 |
| 10:1x | Alt+Tab注入→キャプチャ。**この時の「切り替え完了」判定は誤読**（後述） |
| 10:19 | 独立検証セッション開始時点で、raiseすら元に戻り System Settings がアクティブのままだった |
| 10:20 | サブエージェントが Alt+Tab 再送 → ターミナルが本当にアクティブ化（再現成功） |
| 10:21 | 機能証明: `echo active-term-ok` + Enter を注入 → 画面に `active-term-ok` の実行結果が表示。**キーボードフォーカスがターミナルにある決定的証拠**（20キーペア全ACK `57ab0082010085`） |

## 学んだこと（重要度順）
1. **「前面に raise」と「アクティブ化」は別**。本文クリックで raise できてもフォーカス移動とは限らない。しかも raise は数分で元に戻りうることにある（10:19 でスタック逆戻り観察）
2. **ウィンドウのアクティブ判定の4基準**（キャプチャで読む）:
   - タイトルバーの点灯/グレーアウト
   - 閉じるボタンが青（点灯）かグレーか
   - タスクバーの該当アイコン強調（下線/ボックス）
   - ターミナルのテキストカーソル: **塗りつぶし=フォーカスあり / 中空=なし**
3. **判定は機能テストまでやる**: 見た目の変化だけでは誤読する。`echo <マーカー>` + Enter が画面に出力として現れるか、がフォーカスの決定的証明
4. **絶対マウス(CMD 0x04)の座標配置はターゲット(Cinnamon)で信頼できない**: (249,699) 送りポインタ (430,160) 留まり、等のズレ。Piローカル検証では正常だったのでターゲット側の絶対デバイス扱いの問題と推定。**ターゲットでのクリックは相対移動(CMD 0x05)＋キャプチャで現在位置を測ってから移動量を決める**方式にすること（ボタン押下/解放自体は届く）
5. キーボード注入（CMD 0x02）はターゲットでも完全に信頼できる（Alt+Tab/全アルファベット/スペース/ハイフン/Enter 全て一発成功）

## 再利用可能なスニペット

### キャプチャ（Pi上 → ローカルへ）
```bash
SSH="ssh -F /home/debian/projects/usb-mcp/.ssh/config pi-mcp.lan"
$SSH 'v4l2-ctl -d /dev/video0 --set-fmt-video=width=1280,height=720,pixelformat=MJPG \
  --stream-mmap --stream-count=3 --stream-skip=2 --stream-to=/tmp/v1.jpg'
# ファイル名は毎回新しいものへ（古いroot残骸の上書き失敗を避ける）
$SSH 'cat /tmp/v1.jpg' > /tmp/opencode/verify/v1.jpg
```

### キー注入ハーネス（Pi上で python3）
```python
import os, time, select
fd = os.open("/dev/ttyUSB0", os.O_RDWR)
def drain():
    while True:
        r,_,_ = select.select([fd],[],[],0.5)
        if not r: return
        print("ACK", os.read(fd,16).hex(" "))
def frame(cmd, data):
    f=[0x57,0xAB,0x00,cmd,len(data)]+list(data); f.append(sum(f)&0xff); return f
def send(f, label):
    b=bytes(f); os.write(fd,b); print("TX",label,b.hex(" "))
    time.sleep(0.08); drain()          # 1フレーム=1回のwrite
REL = frame(0x02,[0]*8)                 # 全キー解放（毎回必ず送る）
def key(mod, usage):                    # Alt+Tab = key(0x04, 0x2B)
    send(frame(0x02,[mod,0,usage,0,0,0,0,0]),"press"); time.sleep(0.1); send(REL,"rel")
# usage: a=0x04..z=0x1D, 0x2C=SP, 0x2D='-', 0x28=Enter, 0x2B=Tab
# mod: bit0 LCtrl / bit1 LShift / bit2 LAlt / bit4 LWin
```

### フォーカス機能証明テスト
`echo active-term-ok` + Enter を上記ハーネスで注入 → キャプチャに `active-term-ok` 出力行が出ればフォーカス保証。

## 安全ルール（再確認）
- **CMD 0x07 / 0x0D / 0x0E など仕様外のCMDを絶対に送らない**（V1.4固件でUSB側が消えブートローダ移行。復帰はCH9329のUSBプラグ物理抜去のみ）
- 押下フレームの後必ず全解放フレーム。キー押しっぱなし事故防止
- unknown CMD には 5 バイトの短いエラーACKが返る（チップは無傷。探索は 0x20 之类で）

## 証拠画像
- `captures/frame-20261107-1015-term-focus.jpg` — 誤読された当初「証拠」（実際はSTアクティブのまま、terminal は raise のみ）
- `captures/frame-20261107-1019-before-revert.jpg` — 検証前の現状（STアクティブ/terminal背面・中空カーソル）
- `captures/frame-20261107-1020-alttab-active.jpg` — Alt+Tab後の本当のアクティブ化（タイトルバー点灯・塗りつぶしカーソル）
- `captures/frame-20261107-1021-echo-proof.jpg` — `echo active-term-ok` 実行結果の決定的証明

## 続報: System Settings 閉鎖（11:05-11:35、サブエージェント独立実行で PASS）
- 10:45頃〜 映像が8色カラーバーに復していた。GET_INFO st=0x01（HID生存）から**cinnamon-screensaver/DPMS画面オフ**と推定され、起床手順で復帰実証:
  - 相対マウスジッター（CMD 0x05 `[0x01,00,+5,0,0]`→`[..., -5,0,0]`）1セットでカラーバー→スクリーンセーバー画面（時計11:15）に遷移、Shiftタップ（mod=0x02 のみ。文字キー・CapsLockは不使用）でデスクトップ復帰。**ライブセッションのスクリーンセーバーはロックなし**
- 閉鎖手順: BEFORE で ST アクティブを4点セットの**ピクセル数値実測**で確認（×ボタンの青(16,178,255)クラスタ106px、ターミナルカーソル中空矩形、タスクバーS強調、タイトルバー白(255,255,255)）→ Alt+F4 `57 ab 00 02 08 04 00 3d 00 00 00 00 00 4d` 1発（ACK成功）→ ST消滅（旧×位置の青ピクセル0・タスクバーからS消失）→ `echo st-closed-ok`+Enter（36/36 ACK）実行結果表示でターミナル存活証明
- 副産物の教訓:
  - MJPGはこの時間帯ヘッダ破損フレームが出ることがある（YUYV生は健全）→ **YUYV生フレーム＋PNG変換**で対応。判定はピクセル数値で
  - DATAバイト数をLENより1増やすと `57 ab 00 c2 01 e4 a9`（E4）で拒否。ターゲット注入はゼロ
  - 検証エージェントが「AGENTS.md の SUM 式 `0x10C+k` が誤り」と誤指摘したが、再計算で**既存式が正しい**（公式 'a' 例 SUM=0x10 一致）。文献は変更していない

## 現時点の状態と次にやること（11:35 時点で更新）
- ターゲットのアクティブウィンドウ=ターミナル（単独最前面）。System Settings は閉じ済み
- スクリーンセーバー/DPMS で時折カラーバー化 → 起床手順（相対ジッター+Shiftタップ）で復帰可能。長時間の無操作後はまず疑う
- 未解決: 絶対マウスの座標ズレの正体（Cinnamonの絶対マウス扱い? ボタン解像度? 相対フレームで位置合わせ運用で回避中）
- 未検証: メディアキー(ACPI)のターゲット到達（眠らせ危険のためテストせず） / ターゲット側のLED状態
