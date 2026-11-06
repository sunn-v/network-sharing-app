# Network Sharing

Chọn folder và chia sẻ giữa các máy trong mạng LAN. Bản đầu cho macOS và Windows.

Mở app là thấy các máy cùng mạng. Bấm vào một máy để ghép (lần đầu, đối chiếu mã 6 số), chọn folder chia sẻ cho máy đó, và duyệt, tải folder máy đó chia sẻ cho mình. Không qua server hay internet: tìm máy bằng mDNS, truyền bằng QUIC mã hoá (iroh), chỉ máy đã ghép mới thấy folder.

## Cấu trúc

| Thư mục | Nội dung |
| --- | --- |
| `core/` | Thư viện Rust: tìm máy, ghép, folder chia sẻ, duyệt, tải có resume. Có test. |
| `app/` | App Tauri v2. Giao diện là HTML/JS thuần trong `app/ui/`, không cần bước build frontend. |
| `spike/` | CLI thử mạng ở giai đoạn 0. |

## Chạy và build

Cần Rust (stable) và Node 18+ (chỉ để chạy Tauri CLI).

```sh
cargo test -p lanshare-core          # test core
cargo run -p network-sharing         # chạy app ở chế độ dev
cd app && npx @tauri-apps/cli@2 build  # đóng gói .app/.dmg hoặc .exe/.msi
```

Workflow `build-windows` (chạy tay: `gh workflow run build-windows`) build bản Windows (.exe, .msi) chưa ký để thử.

## LAN Drive on-demand

Folder máy khác chia sẻ hiện sẵn trong Explorer/Finder nhưng chỉ tải khi mở (giống OneDrive/Google Drive). Chỉ đọc: sửa, xoá, đổi tên không đi ngược về máy gốc.

- **Windows** (`app/src/ondemand/windows.rs`): Cloud Files API. `~/LAN Drive` là sync root, hiện trong Explorer kèm biểu tượng trạng thái mây. Build thường (`npx @tauri-apps/cli@2 build`) là có. Lần đầu chạy, nếu `~/LAN Drive` của bản cũ (tải bản cứng) còn đó thì nó được đổi tên thành `LAN Drive (bản cũ)`, không mất file.
- **macOS** (`app/macos/`): File Provider extension viết bằng Swift, hiện ở thanh bên Finder dưới mục Locations. Extension hỏi app qua cầu nối HTTP trên 127.0.0.1 có token (`core/src/bridge.rs`). Phải build bằng `scripts/build-mac.sh` (xem dưới). Bản `cargo run` hoặc build không qua script sẽ không có extension; khi đó LAN Drive quay về kiểu tải bản cứng vào `~/LAN Drive`.
- **Linux**: tải bản cứng.

## Build macOS

Script lấy từ GomokuCafe, như các app getapps khác: ký Developer ID, notarize và staple cả .app lẫn .dmg, mặc định universal (arm64 + x86_64).

Cần: chứng chỉ "Developer ID Application" trong Keychain, Xcode, `brew install xcodegen`, `cargo install tauri-cli`, và hồ sơ notary:

```sh
xcrun notarytool store-credentials "Network SharingNotary" --apple-id ... --team-id VFYA7T675R --password xxxx-xxxx-xxxx-xxxx
echo 'NOTARY_KEYCHAIN_PROFILE="Network SharingNotary"' > .env.local
```

(Chưa có thì lần đầu chạy script hỏi luôn và tự ghi `.env.local`.)

Provisioning profile (một lần, cần `brew install fastlane` và App Store Connect API key trong `.env.local`: `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID`, `APPLE_API_KEY_PATH`):

```sh
./scripts/provision-mac.sh
```

Nó đăng ký 2 App ID (`cafe.getapps.networksharing` và `cafe.getapps.networksharing.LANDrive`) rồi tải profile Developer ID về `app/embedded.provisionprofile` và `app/macos/LANDrive.provisionprofile` (đã gitignore). Đổi bundle ID thì chạy lại.

```sh
./scripts/build-mac.sh                                # build, ký, notarize, rồi tạo GitHub Release
SKIP_RELEASE=1 ./scripts/build-mac.sh                 # chỉ build
./scripts/build-mac.sh --target aarch64-apple-darwin  # một kiến trúc, nhanh hơn
```

Script build extension trước (chưa ký), đóng gói app kèm extension, nhét profile vào cả hai, rồi ký lại extension với entitlements của nó (sandbox, App Group) và ký lại app; nó dừng nếu extension mất `app-sandbox`. Team ID lấy từ tên chứng chỉ, đặt `APPLE_TEAM_ID` nếu muốn khác. DMG nằm ở `target/universal-apple-darwin/release/bundle/dmg/`.

## Lưu ý theo hệ điều hành

- **macOS 15+**: lần đầu mở, hệ thống hỏi quyền Mạng cục bộ (Local Network). Từ chối thì app không thấy máy nào; bật lại trong Cài đặt hệ thống › Quyền riêng tư & Bảo mật › Mạng cục bộ. `app/Info.plist` đã khai báo lý do xin quyền.
- **Windows**: lần đầu mở, Windows Firewall hỏi cho phép kết nối, cần bấm Allow. Mạng đặt profile Public có thể chặn việc tìm máy.

## Dữ liệu

- Cấu hình (khoá máy, máy đã ghép, folder chia sẻ): thư mục app data, file `config.json`.
- File tải về: `~/Downloads/Network Sharing`. Không bao giờ ghi đè file đã có (đặt tên `tên (2).ext`). Tải dở để ở `.nspart` và tự chạy tiếp lần sau.

## Chưa có

Ghi ngược từ LAN Drive về máy gốc, LAN Drive on-demand trên Linux, ký và notarize trong CI. Xem kế hoạch trong tài liệu dự án.
