#!/bin/sh

set -eu

if [ "$#" -eq 0 ]; then
	echo "usage: run-with-openssl.sh command [argument ...]" >&2
	exit 2
fi

openssl_dir="${OPENSSL_DIR:-}"
rustflags="${RUSTFLAGS:-}"
rustdocflags="${RUSTDOCFLAGS:-}"
fallback_dir="${OPENSSL_FALLBACK_DIR:-/tmp/ohara-ossl}"
multiarch="$(cc -print-multiarch 2>/dev/null || true)"
runtime_lib_dir="${OPENSSL_RUNTIME_LIB_DIR:-/usr/lib/$multiarch}"

if [ -z "$openssl_dir" ] && [ ! -e "$runtime_lib_dir/libssl.so" ]; then
	ssl_runtime=
	crypto_runtime=
	for candidate in "$runtime_lib_dir"/libssl.so.*; do
		if [ -f "$candidate" ]; then
			ssl_runtime="$candidate"
			break
		fi
	done
	for candidate in "$runtime_lib_dir"/libcrypto.so.*; do
		if [ -f "$candidate" ]; then
			crypto_runtime="$candidate"
			break
		fi
	done
	if [ -z "$ssl_runtime" ] || [ -z "$crypto_runtime" ]; then
		echo "OpenSSL development libraries not found; install libssl-dev or set OPENSSL_DIR and RUSTFLAGS" >&2
		exit 1
	fi
	mkdir -p "$fallback_dir/lib"
	ln -sfn "$ssl_runtime" "$fallback_dir/lib/libssl.so"
	ln -sfn "$crypto_runtime" "$fallback_dir/lib/libcrypto.so"
	openssl_dir="$fallback_dir"
fi

if [ -n "$openssl_dir" ]; then
	rustflags="$rustflags -L native=$openssl_dir/lib"
	rustdocflags="$rustdocflags -C link-arg=-L$openssl_dir/lib"
fi

export OPENSSL_DIR="$openssl_dir"
export RUSTFLAGS="$rustflags"
export RUSTDOCFLAGS="$rustdocflags"
exec "$@"
