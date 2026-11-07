Name:           corium
Version:        0.1.0~dev
Release:        1%{?buildtag}%{?dist}
Summary:        Static initramfs initializer

License:        Apache-2.0 AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND (BSD-2-Clause OR Apache-2.0 OR MIT) AND (MIT OR Apache-2.0)
URL:            https://github.com/pichi-vm/corium
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz
ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  glibc-static
BuildRequires:  binutils

%description
Static initramfs initializer.

%prep
%autosetup -a1
mkdir -p .cargo
mv cargo-vendor-config.toml .cargo/config.toml

%build
export CARGO_NET_OFFLINE=true
cargo rustc --locked --release --bin corium -- -C target-feature=+crt-static

%install
install -Dpm0755 target/release/corium %{buildroot}%{_bindir}/corium

# Preserve license and notice files from the vendored dependencies.
find vendor -type f \( -iname 'license*' -o -iname 'licence*' -o \
    -iname 'copying*' -o -iname 'notice*' -o -iname 'copyright*' \) \
    -exec install -Dm0644 '{}' '%{buildroot}%{_licensedir}/%{name}/dependencies/{}' \;

%check
# PID 1 runs before shared libraries are mounted.
readelf -l target/release/corium > program-headers.txt
readelf -d target/release/corium > dynamic-section.txt
! grep -q INTERP program-headers.txt
! grep -q NEEDED dynamic-section.txt

%files
%license %{_licensedir}/%{name}/dependencies/
%license LICENSE
%doc README.md
%{_bindir}/corium
