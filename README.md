# monad2steamvr

WiVRn の OpenXR ヘッドセット姿勢を、Rebocap クライアントが VR モードで使う通信プロトコルへ変換する Linux 用 Rust CLI です。`--solarxr` を指定すると、Rebocap が計算したトラッカー姿勢を WiVRn → xrizer → VRChat に公開します。**VRChat OSC は使いません。** HMD・左右コントローラ・Rebocap トラッカーの姿勢は JSON Lines として標準出力にも流します。

**重要:** Rebocap は Windows の名前付きパイプ `\\.\pipe\Rebocap2048VRD` を使います。この CLI は Wine 内で動く [rebocap-linux-compat の bridge.exe](https://github.com/dogesoulseller/rebocap-linux-compat) に TCP で接続します。**SteamVR 本体の仮想 HMD・トラッカードライバではありません。** Rebocap の解析済みプロトコル（v02 beta02）と、WiVRn 26.9 が使う Monado `f037264d23e2472a444a157370647fcd601ed81b` の SolarXR IPC に対応しています。公開先には SolarXR 有効の WiVRn と、`monado` 機能有効の xrizer（0.5 は既定で有効）が必要です。通信互換性はテストしていますが、Rebocap 実機と VRChat での動作確認は別途必要です。

## 開発・ビルド

```sh
nix develop
cargo build --release
cargo test
```

Nix flake の開発環境は rust-overlay の Rust と OpenXR loader、および x86_64 Linux では Wine 用 `bridge.exe` を提供します。`bridge.exe` は [rebocap-linux-compat](https://github.com/dogesoulseller/rebocap-linux-compat) の MIT ライセンスのソースを固定したリビジョンからクロスコンパイルします。実行時には WiVRn サーバーと接続済みヘッドセットが必要です。WiVRn の OpenXR ランタイムが `XR_MND_headless`、`XR_KHR_convert_timespec_time` と `STAGE` 基準空間を公開している必要があります。headless セッションではフレーム API を使わず、ランタイムの現在時刻で姿勢を取得します。

## 使用方法

1. WiVRn のサーバーを起動し、ヘッドセットを接続します。WiVRn の OpenXR ランタイムが有効か確認します。
2. `nix develop` で開発シェルに入ります（初回は `bridge.exe` もビルドされます）。
3. Rebocap クライアントと **同じ Wine prefix** で `bridge.exe` を起動し、**起動したまま**にします。たとえば [rebocap-linux](https://github.com/akazdayo/rebocap-linux) の既定 prefix の場合:

   ```sh
   WINEPREFIX="${XDG_DATA_HOME:-$HOME/.local/share}/rebocap/wineprefix" wine "$(command -v bridge.exe)"
   ```

4. Rebocap クライアントを起動した状態で、別のターミナルから実行します:

   ```sh
   nix develop -c cargo run --release -- --bridge 127.0.0.1:36850
   ```

Rebocap クライアントが作成するパイプを bridge.exe が開きます。初期応答では HMD の接続状態、ゼロオフセットの立位座標系、頭部姿勢を送信します。Rebocap からのトラッカー姿勢も標準出力に出ます。ステータス・接続エラーは標準エラー出力に出ます。

出力例（1 行 1 JSON オブジェクト）:

```json
{"device":"headset","id":0,"role":null,"name":null,"connected":true,"battery":null,"pose":{"position":[0.0,1.7,0.0],"rotation":[1.0,0.0,0.0,0.0]}}
{"device":"tracker","id":3,"role":0,"name":"rebocap_waist","connected":true,"battery":0.8,"pose":{"position":[0.0,1.0,0.0],"rotation":[1.0,0.0,0.0,0.0]}}
```

位置はメートル単位で +Y が上、-Z が前、回転は `[w,x,y,z]` の順番です。コントローラの姿勢は標準出力に公開しますが、Rebocap の既知のプロトコルでは HMD の姿勢だけを入力として使います。ヘッドセットと実機を使った接続確認は環境ごとに必要です。

## Rebocap が認識しない場合

- `right_controller` / `left_controller` の JSON は **この CLI の出力**です。Rebocap へ送る入力は `headset` の姿勢だけなので、まず `headset` の JSON で `connected: true` になっているか確認してください。
- 標準エラーに `WiVRn headset tracking valid: true` と `Sent Rebocap headset status and standing-space handshake to TCP bridge` が出るか確認してください。後者は **TCP への書き込み成功**であり、Wine パイプへの到達確認ではありません。
- bridge.exe の端末に `listening on 127.0.0.1:36850`、`driver connected, opening ...`、**`pipe open, relaying`** の順に出るか確認してください。最後が出なければ Rebocap クライアントの起動状態・Wine prefix の一致を確認してください。
- Rebocap からの `tracker_added` などが届くと CLI は `Received a Rebocap protocol message via the Wine pipe` と記録します。Rebocap は PC モードでは返信しません。VR モードでもトラッカー位置はキャリブレーション後に届きます。
- `Connection refused` は bridge.exe が TCP ポートで待ち受けていない状態です。Rebocap の画面表示は標準出力の JSON とは連動しません。

参考: [WiVRn の OpenVR/SteamVR に関する資料](https://github.com/WiVRn/WiVRn/blob/master/docs/steamvr.md)、[Rebocap 通信仕様の解析](https://github.com/dogesoulseller/rebocap-linux-compat/blob/main/docs/protocol.md)。

## VRChat 用の SolarXR トラッカー公開

```text
WiVRn HMD → OpenXR → この CLI → bridge.exe → Rebocap
                       ↑                       │
                       └──── 計算済み姿勢 ───────┘
                       │
                       └→ SolarXR IPC → WiVRn → xrizer → VRChat GenericTracker
```

**上の通常使用方法とは起動順が異なります。CLI を先に起動してからヘッドセットを接続してください。** WiVRn はヘッドセット接続時に SolarXR のトラッカー一覧を一度だけ取得します。CLI は OpenXR の接続を待つ前に一覧を提供するので、HMD がない状態でも登録できます。

1. WiVRn サーバーを起動します。ヘッドセットはまだ接続しません。接続中なら切断してください。
2. 前述の手順で Rebocap と同じ Wine prefix の `bridge.exe`、Rebocap クライアントを起動します。
3. 同じ Linux ユーザーで CLI を起動します。

   ```sh
   nix develop -c cargo run --release -- --solarxr --bridge 127.0.0.1:36850
   ```

4. 標準エラーに `SolarXR ready at .../SlimeVRRpc` が出たら、WiVRn ヘッドセットを接続します。OpenXR セッションがまだ作れない間も SolarXR は待ち受けを続けます。
5. Rebocap を VR モードにし、キャリブレーションします。姿勢取得前のトラッカーは登録済みでも姿勢無効です。
6. xrizer 経由で VRChat を起動し、FBT キャリブレーションを実行します。

既定では腰・左右足の 3 点を登録します。必要な部位を `--tracker-roles` に指定できます。**登録部位の変更や CLI の再起動後は、ヘッドセットを再接続してください。**

| Rebocap role | 部位 | SolarXR BodyPart |
|---|---|---|
| 0 | 腰 | WAIST |
| 1 / 2 | 左右上腿（膝用） | LEFT / RIGHT_UPPER_LEG |
| 3 / 4 | 左右下腿（足首用） | LEFT / RIGHT_LOWER_LEG |
| 5 / 6 | 左右足 | LEFT / RIGHT_FOOT |
| 7 | 胸 | CHEST |
| 9 / 10 | 左右上腕（肘用） | LEFT / RIGHT_UPPER_ARM |
| 11 / 12 | 左右前腕 | LEFT / RIGHT_LOWER_ARM |

全対応部位を登録する例:

```sh
nix develop -c cargo run --release -- --solarxr --tracker-roles 0,1,2,3,4,5,6,7,9,10,11,12
```

トラッカーの SolarXR ID は role + 3 で固定します。実際の Rebocap ID と role の対応は `tracker_added` から取得するので、Rebocap の ID が変わっても公開側のシリアルは安定します。ヘッド role 8 とコントローラ姿勢置換には対応しません。SlimeVR サーバーは不要です。

### 座標・追跡状態

- Rebocap の計算済み姿勢をそのまま使い、身体モデルの再計算や平滑化はしません。SolarXR では synthetic tracker として公開します。
- WiVRn 26.9 の通常の STAGE / HMD tracking origin を前提に、位置はメートル、+Y 上、-Z 前。クォータニオンだけ `[w,x,y,z]` → `[x,y,z,w]` に並べ替えて正規化します。
- 位置がずれる場合は出力側だけ補正できます。`--solarxr-yaw` は +Y 軸回りの度数、`--solarxr-offset` は **回転後**に加えるメートル単位の平行移動です。

  ```sh
  nix develop -c cargo run --release -- --solarxr --solarxr-yaw 5 --solarxr-offset 0,0.02,0
  ```

- `OXR_RECENTER_STAGE=1` や別途 tracking origin を変更する構成は、この既定の座標一致の対象外です。まず通常の WiVRn STAGE で確認してください。ヘッドセットのリセンター後は Rebocap / VRChat を再キャリブレーションしてください。
- status が OK で、status が 3 秒以内、姿勢が 500 ms 以内のときだけ有効な姿勢を送ります。Rebocap の切断、無効な数値、更新停止では姿勢を無効化します。
- 固定版 Monado は SolarXR の status を無視するため、無効時は「速度だけを持ち、位置・回転を持たないサンプル」を送って古い姿勢履歴を無効化します。xrizer 上のデバイス登録は残りますが、姿勢は無効になります。
- Ctrl+C / SIGTERM でも最終無効化を送信し、作成したソケットを削除します。

### ソケットと診断

`$XDG_RUNTIME_DIR/SlimeVRRpc` と `SlimeVRInput` に、このユーザーだけがアクセスできる Unix ソケットを作ります。後者は WiVRn の HMD・コントローラ入力を消費するためのもので、Rebocap への HMD 入力は従来どおり OpenXR を使います。TCP ポートの追加や `~/configs` の WiVRn 設定変更は不要です。

- `XDG_RUNTIME_DIR` は通常のユーザーログイン環境で設定されます。WiVRn と同じユーザーで実行してください。
- `SolarXR runtime connected` と、WiVRn の `Enumerated 3 SolarXR synthetic trackers` を確認してください。後者の数は登録部位数です。
- xrizer のログでは `Creating ... generic trackers via Monado XDev extension` を確認してください。
- `cannot bind ... Address already in use` は SlimeVR、別の CLI、または異常終了時に残ったソケットとの競合です。既存ソケットは自動削除しません。該当プロセスが終了していることを確認し、この CLI が残したものだけ削除して再起動してください。
- CLI のクラッシュや SIGKILL では最終無効化を送れません。固定版 Monado 自体にはこの切断時の姿勢失効処理がないため、ヘッドセットを切断・再接続してください。

### 通信互換性テスト

通常の `cargo test` は Unix ソケット上での登録・ストリーミング、分割フレーム、不正入力、切断・タイムアウト処理を確認します。固定 Monado の **実際の C パーサー**でも登録・姿勢・無効化のフレームを検証できます:

```sh
MONADO_SOURCE=/path/to/monado/source nix develop -c cargo test monado_wire_compatibility -- --ignored
```

`MONADO_SOURCE` は WiVRn 26.9 と同じリビジョンのソースを指定してください。このテストは開発シェルの C コンパイラを使用します。
