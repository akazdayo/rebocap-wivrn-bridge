# monad2steamvr

Linux の WiVRn + xrizer 環境で、Rebocap を VRChat のフルボディトラッキングに使うための CLI です。VRChat OSC は使わず、通常のトラッカーとして姿勢を渡します。

[rebocap-linux](https://github.com/akazdayo/rebocap-linux) で Rebocap を動かす環境を前提に開発・動作確認しています。手順中の Wine prefix もその既定設定です。rebocap-linux 自体への直接の依存はなく、同等の Wine 環境でも利用できます。

```text
WiVRn の頭部姿勢 → この CLI → Wine ブリッジ → Rebocap
Rebocap のトラッカー姿勢 → この CLI → SolarXR → WiVRn → xrizer → VRChat
```

## 必要なもの

- x86_64 Linux、Wine
- WiVRn + xrizer と接続できるヘッドセット
- rebocap-linux でセットアップした Rebocap クライアントとセンサー

WiVRn 26.9 + xrizer 0.5 で動作確認済みです。WiVRn の SolarXR と xrizer の `monado` 機能を使用します（この組み合わせでは既定で有効）。

## 使い方

WiVRn サーバーと Rebocap を起動し、ヘッドセットはまだ接続せずに、次の順で進めます。

### 1. Wine ブリッジを起動する

[rebocap-linux-compat](https://github.com/dogesoulseller/rebocap-linux-compat) の `bridge.exe` を用意し、Rebocap と同じ Wine prefix で起動します。

```sh
WINEPREFIX="${XDG_DATA_HOME:-$HOME/.local/share}/rebocap/wineprefix" wine ./bridge.exe
```

この端末は起動したままにします。

### 2. 別の端末で CLI を起動する

```sh
cargo run --release -- --solarxr
```

`SolarXR ready` が表示されたら、ヘッドセットを接続・装着します。

### 3. キャリブレーションする

Rebocap を VR モードにしてキャリブレーションし、xrizer 経由で VRChat を起動して FBT キャリブレーションを行います。既定では **腰・左右足の 3 点**を公開します。

**CLI を再起動したときや公開部位を変更したときは、ヘッドセットを再接続して VRChat も再起動してください。** WiVRn はヘッドセット接続時にトラッカー一覧を取得します。

## オプション

| オプション | 用途 | 既定値 |
|---|---|---|
| `--solarxr` | WiVRn にトラッカーを公開 | 無効 |
| `--tracker-roles` | 公開する部位をカンマ区切りで指定 | `0,5,6` |
| `--bridge` | Wine ブリッジの接続先 | `127.0.0.1:36850` |
| `--solarxr-offset X,Y,Z` | 出力位置の補正（メートル、回転後に加算） | `0,0,0` |
| `--solarxr-yaw DEGREES` | 出力方向の補正（+Y 軸回りの度数） | `0` |

部位番号は `0`: 腰、`1/2`: 左右上腿、`3/4`: 左右下腿、`5/6`: 左右足、`7`: 胸、`9/10`: 左右上腕、`11/12`: 左右前腕です。

全対応部位を公開する例:

```sh
cargo run --release -- --solarxr --tracker-roles 0,1,2,3,4,5,6,7,9,10,11,12
```

姿勢は JSON Lines として標準出力にも流れます。位置はメートル単位、+Y が上、-Z が前、回転は `[w,x,y,z]` です。接続ログは標準エラーに出ます。

## うまく動かないとき

| 症状 | 確認すること |
|---|---|
| Rebocap が SteamVR を認識しない | ブリッジに `pipe open, relaying`、CLI に `WiVRn headset tracking valid: true` が出ているか。Wine prefix も確認 |
| Rebocap の姿勢が届かない | VR モードでキャリブレーション済みか。CLI に `Received a Rebocap protocol message` が出ているか |
| VRChat がトラッカーを認識しない | CLI に `SolarXR runtime connected` が出ているか。CLI を動かしたままヘッドセットを再接続し、VRChat を再起動 |

## 開発

ビルドには Rust と OpenXR loader の開発用ライブラリが必要です。

```sh
cargo build --release
cargo test
```

参考: [WiVRn と OpenVR](https://github.com/WiVRn/WiVRn/blob/master/docs/steamvr.md) · [Rebocap 通信仕様](https://github.com/dogesoulseller/rebocap-linux-compat/blob/main/docs/protocol.md)

## 外部コンポーネントのライセンス

Wine ブリッジ `bridge.exe` は [rebocap-linux-compat](https://github.com/dogesoulseller/rebocap-linux-compat) のコードを使用しています。Copyright (c) 2026 Marcin Czerwonka、MIT License。全文は [LICENSES/rebocap-linux-compat.txt](LICENSES/rebocap-linux-compat.txt) を参照してください。
