# 備忘録: マウス操作の挙動検証（2026-11-08）

> AGENTS.md から移動した詳細記録。CH9329 経由のマウス注入に関する実測・数値検証ログ。

## 結論（pi-mcp の hid_mouse_* が「怪しい」正体）

1. **ターゲット既定のポインタ加速（libinput adaptive）が最大の元凶**。
   - `gsettings get org.cinnamon.desktop.peripherals.mouse accel-profile` は既定で `'default'`（=adaptive）。単一イベントの delta が大きいほど 1.7〜2.0 倍に増幅され、+1〜2px は素通りする速度依存の非線形挙動。
   - **修正済み（このセッションのターゲット）**: `gsettings set org.cinnamon.desktop.peripherals.mouse accel-profile flat` → 以後 delta は 1:1（Xlib query_pointer で実測一致: +50,+50→厳密、30,30×5 回全一致、(64,64)(-30,-20)(10,5)(40,40) 合計 84,89 完全一致）。
   - **ライブセッションのみ。ターゲット再起動のたびに再設定すること**（runbook 化推奨）。

2. **ホイール（CMD 0x05 のホイールバイト）はターゲット側で REL_WHEEL にならない**。
   - Linux Mint 22 (kernel 6.8.0-38) の event5 ダンプでは `EV_ABS(ABS_X=v, ABS_Y=120v)` として届き、スクロールも起きなければ ABS ワープも起きない（完全な no-op）。
   - Pi 側 (6.12) で REL_WHEEL として観測した 2026-11-07 の `third-party/wch/ch9329-spec.md` §8 実測はカーネル側の扱い違い。
   - **xterm でのスクロールは `hid_key_press` の Shift+Prior/Next で代用**。

3. **絶対移動（CMD 0x04 / hid_mouse_abs_move）は動く瞬間と失効する瞬間がある**。
   - c85 実測: abs(640,360) → xterm 右クリックメニューがウィンドウ内に出る = ワープ成功。
   - 一方 c69 では ABS_X=2048, ABS_Y=2048 が kernel には届いたが X ポインタは不変。
   - **単発 abs は有効だが間欠的にフレームが消える**。使用後は必ずキャプチャ／P で確認。

4. **間欠的な偽フレーム（モジュール起因の spoiling）**。
   - event5 ダンプに pin バーストの 630ms 後へ `REL(-92,-92)` の幻イベントが混入（送信フレームは全て ACK OK＝UART チェックサム通過後、モジュール USB 側かターゲット USB で汚染／再生成されている）。
   - ±30px 前後の「置いたはずの位置と違う」はこれで説明がつく。大きい移動後は視覚／P で着地点確認が必須。

## ターゲット上の WCH 機器構成（実測）

- event2 = `WWW.WCH.CN WCH UART TO KB-MS_V1.4`（キーボード）
- event3 = System Control
- event4 = Consumer Control
- event5 = ...V1.4 Mouse（相対ホイール系）
- mouse0 = event5 の mousedev ノード
- xinput は未インストール。python3 と python3-Xlib は標準で使える。

### ポインタ位置のグラウンドトゥルース（P 関数）

```bash
P(){
  python3 -c 'from Xlib import display; d=display.Display(); w=d.get_input_focus().focus; p=w.query_pointer(); print(p.root_x, p.root_y)'
}
```

### kernel 生イベントの取り込み

```bash
echo o | sudo -S sh -c 'timeout 8 cat /dev/input/event5 > /tmp/e5.bin'
cat > /tmp/d.py <<'E'
import sys, struct
fmt='<qqHHi'
with open(sys.argv[1],'rb') as f:
  while True:
    b=f.read(24)
    if len(b)<24: break
    print(struct.unpack(fmt,b))
E
python3 /tmp/d.py /tmp/e5.bin
```

注意: **sudo コマンド実行中に type_text した入力は sudo の tty flush で消える**（実行完了後に打つこと）。

## 測定にはまった落とし穴（将来の検証エージェント向け）

- Cinnamon はアイドル時カーソル矢印を隠す（**2〜3 秒不動だとキャプチャに矢印が映らない。注入失敗ではない**）。動かすと再出現。
- 右クリックメニュー左上 ≠ ポインタ位置がアプリで違う:
  - nemo デスクトップメニュー = +(0,+1)
  - **xterm メニュー = (−10,−64) 相当（上方向にずれて出る）**
- デスクトップ上での右クリック／左クリックはターミナルを非アクティブ化する（Esc では戻らない）。**メニュー操作後はターミナル本文を左クリックしてフォーカス復帰**（これをしていないと「キーボード注入が死んだ」と誤解する。`echo MK50` が消え、復帰後 `echo MK51`→`cho MK51` と 1 文字落ち observed 実例）。
- MCP 呼び出し間は 0.3〜0.5 秒。shell 変数関数の python 起動（~0.5-1s）と次の hid_mouse_* が競合して P の読みが次の pin に壊される。**P の後は capture_frame を挟んで直列化**すること。
- MJPEG(JPEG) キャプチャはこの時間帯カラーバー固定フレームを返すことがあり（c72-76）、PNG(YUYV) で実映像確認を併用。
