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
- **macOS** (`app/macos/`): File Provider extension viết bằng Swift, hiện ở thanh bên Finder dưới mục Locations. Extension hỏi app qua cầu nối HTTP trên 127.0.0.1 có token (`core/src/bridge.rs`). Phải build bằng script:
  ```sh
  brew install xcodegen
  export APPLE_TEAM_ID=XXXXXXXXXX
  export APPLE_SIGNING_IDENTITY="Developer ID Application: Tên (XXXXXXXXXX)"
  app/macos/build.sh
  ```
  Kiểm tra extension giữ đúng entitlements sau khi đóng gói:
  `codesign -d --entitlements - "target/release/bundle/macos/Network Sharing.app/Contents/PlugIns/LANDrive.appex"` phải có `app-sandbox` và `application-groups`.
  Bản `cargo run` hoặc build không qua script sẽ không có extension; khi đó LAN Drive quay về kiểu tải bản cứng vào `~/LAN Drive`.
- **Linux**: tải bản cứng.

## Lưu ý theo hệ điều hành

- **macOS 15+**: lần đầu mở, hệ thống hỏi quyền Mạng cục bộ (Local Network). Từ chối thì app không thấy máy nào; bật lại trong Cài đặt hệ thống › Quyền riêng tư & Bảo mật › Mạng cục bộ. `app/Info.plist` đã khai báo lý do xin quyền.
- **Windows**: lần đầu mở, Windows Firewall hỏi cho phép kết nối, cần bấm Allow. Mạng đặt profile Public có thể chặn việc tìm máy.

## Dữ liệu

- Cấu hình (khoá máy, máy đã ghép, folder chia sẻ): thư mục app data, file `config.json`.
- File tải về: `~/Downloads/Network Sharing`. Không bao giờ ghi đè file đã có (đặt tên `tên (2).ext`). Tải dở để ở `.nspart` và tự chạy tiếp lần sau.

## Chưa có

Ghi ngược từ LAN Drive về máy gốc, LAN Drive on-demand trên Linux, ký và notarize trong CI. Xem kế hoạch trong tài liệu dự án.
