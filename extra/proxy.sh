#!/bin/sh
set -eux

# This script is written to be as POSIX as possible
# so it works fine for all Unix-like operating systems

test_cmd() {
  command -v "$1" >/dev/null
}

# proxy version
planck_new_ver="${1}"
# proxy directory
# eval to resolve '~' into proper user dir
eval planck_dir="'${2}'"

case "${planck_new_ver}" in
  v*)
    planck_new_version=$(echo "${planck_new_ver}" | cut -d'v' -f2)
    planck_new_ver_tag="${planck_new_ver}"
  ;;
  nightly*)
    planck_new_version="${planck_new_ver}"
    planck_new_ver_tag=$(echo ${planck_new_ver} | cut -d '-' -f1)
  ;;
  *)
    printf 'Unknown version\n'
    exit 1
  ;;
esac

if [ -e "${planck_dir}/planck" ]; then
  planck_installed_ver=$("${planck_dir}/planck" --version | cut -d' ' -f2)

  printf '[DEBUG]: Current proxy version: %s\n' "${planck_installed_ver}"
  printf '[DEBUG]: New proxy version: %s\n' "${planck_new_version}"
  if [ "${planck_installed_ver}" = "${planck_new_version}" ]; then
    printf 'Proxy already exists\n'
    exit 0
  else
    printf 'Proxy outdated. Replacing proxy\n'
    rm "${planck_dir}/planck"
  fi
fi

for _cmd in tar gzip uname; do
  if ! test_cmd "${_cmd}"; then
    printf 'Missing required command: %s\n' "${_cmd}"
    exit 1
  fi
done

# Currently only linux/darwin are supported
case $(uname -s) in
  Linux) os_name=linux ;;
  Darwin) os_name=darwin ;;
  *)
    printf '[ERROR] unsupported os\n'
    exit 1
  ;;
esac

# Currently only amd64/arm64 are supported
case $(uname -m) in
  x86_64|amd64|x64) arch_name=x86_64 ;;
  arm64|aarch64) arch_name=aarch64 ;;
  # riscv64) arch_name=riscv64 ;;
  *)
    printf '[ERROR] unsupported arch\n'
    exit 1
  ;;
esac

planck_download_url="https://github.com/alejandroqh/planck/releases/download/${planck_new_ver_tag}/planck-proxy-${os_name}-${arch_name}.gz"

printf 'Creating "%s"\n' "${planck_dir}"
mkdir -p "${planck_dir}"
cd "${planck_dir}"

if test_cmd 'curl'; then
  # How old curl has these options? we'll find out
  printf 'Downloading using curl\n'
  curl --proto '=https' --tlsv1.2 -LfS -O "${planck_download_url}"
  # curl --proto '=https' --tlsv1.2 -LZfS -o "${tmp_dir}/planck-proxy-${os_name}-${arch_name}.gz" "${planck_download_url}"
elif test_cmd 'wget'; then
  printf 'Downloading using wget\n'
  wget "${planck_download_url}"
else
  printf 'curl/wget not found, failed to download proxy\n'
  exit 1
fi

printf 'Decompressing gzip\n'
gzip -df "${planck_dir}/planck-proxy-${os_name}-${arch_name}.gz"

printf 'Renaming proxy \n'
mv -v "${planck_dir}/planck-proxy-${os_name}-${arch_name}" "${planck_dir}/planck"

printf 'Making it executable\n'
chmod +x "${planck_dir}/planck"

printf 'planck-proxy installed\n'

exit 0
