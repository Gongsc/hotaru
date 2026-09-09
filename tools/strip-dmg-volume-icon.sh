#!/usr/bin/env bash
#
# 从 dmg 里删掉 .VolumeIcon.icns。
#
# Tauri 打包用的 bundle_dmg.sh 会把应用图标复制成卷根目录下的
# .VolumeIcon.icns 来设置卷图标。它只是个点开头的文件,没有任何隐藏标志;
# Finder 一旦开着"显示隐藏文件",它就会作为一个多余图标出现在安装窗口里
# (.DS_Store 不会,因为 Finder 把自己的元数据文件单独过滤掉了)。
#
# 补隐藏标志(chflags hidden / SetFile -a V)对开了显示隐藏文件的 Finder
# 无效 —— 试过,照样显示。所以直接删掉这个文件,并清掉卷根目录的自定义图标
# 属性(C),装载后的窗口里就只剩 Hotaru.app 和 Applications。
#
# 代价:装载后的卷用系统默认磁盘图标,不再是应用图标。
#
# 用法:tools/strip-dmg-volume-icon.sh <dmg> [<dmg> ...]
# 需要在 macOS 上运行;dmg 会被就地替换。

set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "用法: $0 <dmg> [<dmg> ...]" >&2
  exit 2
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "只能在 macOS 上运行" >&2
  exit 2
fi

work=$(mktemp -d)
mount_point=""

cleanup() {
  if [[ -n "$mount_point" && -d "$mount_point" ]]; then
    hdiutil detach "$mount_point" -quiet 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

for dmg in "$@"; do
  if [[ ! -f "$dmg" ]]; then
    echo "找不到 $dmg" >&2
    exit 1
  fi
  echo "处理 $dmg"

  rw="$work/rw.dmg"
  out="$work/out.dmg"
  mount_point="$work/mnt"
  rm -f "$rw" "$out"

  # 压缩后的镜像不能直接写,先转成可读写格式
  hdiutil convert "$dmg" -format UDRW -o "$rw" -quiet
  mkdir -p "$mount_point"
  hdiutil attach "$rw" -nobrowse -owners off -mountpoint "$mount_point" -quiet

  if [[ -f "$mount_point/.VolumeIcon.icns" ]]; then
    rm -f "$mount_point/.VolumeIcon.icns"
    # 图标文件没了就把卷的自定义图标属性一起清掉,免得 Finder 去找一个不存在的图标
    if command -v SetFile >/dev/null 2>&1; then
      SetFile -a c "$mount_point"
    else
      echo "  未找到 SetFile,卷的自定义图标属性保持原样" >&2
    fi
    echo "  已删除 .VolumeIcon.icns"
  else
    echo "  卷里没有 .VolumeIcon.icns,跳过"
  fi

  hdiutil detach "$mount_point" -quiet
  mount_point=""

  hdiutil convert "$rw" -format UDZO -imagekey zlib-level=9 -o "$out" -quiet
  mv "$out" "$dmg"
  echo "  完成"
done
