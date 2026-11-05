# network-sharing-app

Chọn folder và chia sẻ giữa các máy trong mạng LAN (macOS, Windows trước; Linux sau).

## Giai đoạn 0: spike mạng (`spike/`)

CLI nhỏ để kiểm tra phần khó nhất: hai máy có tự thấy nhau qua mDNS và truyền file nhanh qua iroh (QUIC) không.
Chỉ chạy trong LAN: preset `Minimal` của iroh không dùng relay hay DNS bên ngoài.

```sh
cd spike
cargo test
cargo build --release

# Máy nhận
LANSHARE_NAME=mac ./target/release/lanshare-spike serve ~/Downloads/lanshare

# Máy gửi (Windows: set LANSHARE_NAME=win)
./target/release/lanshare-spike list
./target/release/lanshare-spike send file-1GB.bin mac
```

Bản build sẵn cho macOS và Windows có trong artifact của workflow `spike` trên GitHub Actions.

### Cần kiểm tra trên máy thật

- [ ] Mac thấy Windows và ngược lại (`list`), cùng Wi-Fi và qua dây.
- [ ] Gửi 1 GB, tốc độ gần trần LAN (khoảng 100 MB/s trên gigabit).
- [ ] macOS hiện hộp thoại Local Network; từ chối thì `list` trống, cho phép lại thì chạy.
- [ ] Windows hiện hộp thoại Firewall; mạng để profile Public thì có thấy nhau không.
