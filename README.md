# monad2steamvr

WiVRn の OpenXR ヘッドセット姿勢を、Rebocap クライアントが VR モードで使う通信プロトコルへ変換する Linux 用 Rust CLI です。WiVRn の HMD と左右コントローラ、および Rebocap が計算したトラッカーの姿勢を JSON Lines として標準出力に流します。

**重要:** Rebocap は Windows の名前付きパイプ `\\.\pipe\Rebocap2048VRD` を使います。この CLI は Wine 内で動く [rebocap-linux-compat の bridge.exe](https://github.com/dogesoulseller/rebocap-linux-compat) に TCP で接続します。**SteamVR 本体の仮想 HMD・トラッカードライバではありません。** 解析済みの Rebocap プロトコル（v02 beta02）で頭部姿勢を送ってトラッカーの返信を受け取る実装です。Rebocap のすべてのバージョンで VR モードとして認識されることや、トラッカーの SteamVR / VRChat への登録は保証しません。VRChat 側で使うには別途トラッカー公開先の連携が必要です。

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
