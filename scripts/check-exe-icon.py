"""只读核对 Windows EXE 内嵌图标与源 ICO 的全部七种尺寸。@author ahui

使用 Python 标准库 ctypes，以数据/资源模式加载 EXE，不启动进程、不执行入口，
不修改 EXE、注册表、系统图标缓存或用户数据。默认检查 dist/batch-send-email.exe
中的 Tauri 资源组 32512，逐帧与 src-tauri/icons/icon.ico 的图像字节比较。

执行 python scripts/check-exe-icon.py；可用 --exe dist/其他目录/batch-send-email.exe
指定另一个已生成的交付包。此检查证明新图标已进入 EXE，不能代替原生窗口或
任务栏的实际显示验收；Windows Shell 也可能保留旧文件路径的图标缓存。
"""

from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
from pathlib import Path
import struct
import sys


ROOT = Path(__file__).resolve().parents[1]
EXPECTED_SIZES = {16, 24, 32, 48, 64, 128, 256}
APP_ICON_RESOURCE_ID = 32512
RT_ICON = 3
RT_GROUP_ICON = 14
LOAD_LIBRARY_AS_DATAFILE = 0x00000002
LOAD_LIBRARY_AS_IMAGE_RESOURCE = 0x00000020


def ico_images(path: Path) -> dict[tuple[int, int], bytes]:
    """读取 ICO 的尺寸和原始图像字节，不进行解码或重新压缩。

    Args:
        path: 仓库源 ICO 路径，只读；预期包含七个不重复正方形尺寸。

    Returns:
        以实际像素宽高为键的图像数据；ICO 中宽高字节 0 按规范表示 256。

    Raises:
        OSError: 文件不可读。
        ValueError: ICO 头、目录、范围或尺寸不符合当前项目要求。
    """
    data = path.read_bytes()
    if len(data) < 6:
        raise ValueError("源 ICO 文件过短。")
    reserved, kind, count = struct.unpack_from("<HHH", data)
    if reserved != 0 or kind != 1 or len(data) < 6 + count * 16:
        raise ValueError("源 ICO 文件头或目录无效。")
    images: dict[tuple[int, int], bytes] = {}
    for index in range(count):
        width, height, _colors, _reserved, _planes, _bits, size, offset = (
            struct.unpack_from("<BBBBHHII", data, 6 + index * 16)
        )
        dimensions = (width or 256, height or 256)
        if dimensions in images or not size or offset < 6 + count * 16:
            raise ValueError("源 ICO 有重复尺寸或无效图像范围。")
        if offset + size > len(data):
            raise ValueError("源 ICO 图像超出文件范围。")
        images[dimensions] = data[offset : offset + size]
    expected = {(size, size) for size in EXPECTED_SIZES}
    if set(images) != expected:
        raise ValueError("源 ICO 必须包含 16/24/32/48/64/128/256 七种尺寸。")
    return images


def check_executable(executable: Path, expected: dict[tuple[int, int], bytes]) -> None:
    """按 Win32 资源 API 读取 EXE 图标组及图像，与源 ICO 逐字节比对。

    Args:
        executable: 已生成的 Windows EXE 绝对路径，不启动或执行该文件。
        expected: 已通过 ICO 格式校验的七种图像，键为宽高像素。

    Raises:
        OSError: 文件不可读、资源不存在或 Windows API 失败。
        ValueError: 资源组类型、尺寸或图像内容与源 ICO 不一致。
    """
    if sys.platform != "win32":
        raise OSError("EXE 资源校验只支持 Windows；未在其他系统执行替代检查。")
    if not executable.is_file():
        raise OSError(f"找不到待检查 EXE：{executable}")

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LoadLibraryExW.argtypes = [wintypes.LPCWSTR, wintypes.HANDLE, wintypes.DWORD]
    kernel.LoadLibraryExW.restype = wintypes.HMODULE
    kernel.FindResourceW.argtypes = [wintypes.HMODULE, ctypes.c_void_p, ctypes.c_void_p]
    kernel.FindResourceW.restype = wintypes.HRSRC
    kernel.SizeofResource.argtypes = [wintypes.HMODULE, wintypes.HRSRC]
    kernel.SizeofResource.restype = wintypes.DWORD
    kernel.LoadResource.argtypes = [wintypes.HMODULE, wintypes.HRSRC]
    kernel.LoadResource.restype = wintypes.HGLOBAL
    kernel.LockResource.argtypes = [wintypes.HGLOBAL]
    kernel.LockResource.restype = ctypes.c_void_p
    kernel.FreeLibrary.argtypes = [wintypes.HMODULE]
    kernel.FreeLibrary.restype = wintypes.BOOL

    # 两个标志确保只映射资源，Windows 不运行目标 EXE 的入口或依赖初始化。
    module = kernel.LoadLibraryExW(
        str(executable), None, LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE
    )
    if not module:
        raise ctypes.WinError(ctypes.get_last_error())

    def resource_bytes(resource_id: int, resource_type: int) -> bytes:
        """在当前有效模块句柄内复制资源字节；释放模块后不保留任何指针。"""
        resource = kernel.FindResourceW(
            module, ctypes.c_void_p(resource_id), ctypes.c_void_p(resource_type)
        )
        if not resource:
            raise ctypes.WinError(ctypes.get_last_error())
        size = kernel.SizeofResource(module, resource)
        loaded = kernel.LoadResource(module, resource)
        if not size or not loaded:
            raise ctypes.WinError(ctypes.get_last_error())
        pointer = kernel.LockResource(loaded)
        if not pointer:
            raise ctypes.WinError(ctypes.get_last_error())
        return ctypes.string_at(pointer, size)

    try:
        group = resource_bytes(APP_ICON_RESOURCE_ID, RT_GROUP_ICON)
        if len(group) < 6:
            raise ValueError("EXE 图标资源组过短。")
        reserved, kind, count = struct.unpack_from("<HHH", group)
        if reserved != 0 or kind != 1 or len(group) != 6 + count * 14:
            raise ValueError("EXE 图标资源组格式无效。")
        seen: set[tuple[int, int]] = set()
        for index in range(count):
            width, height, _colors, _reserved, _planes, _bits, size, resource_id = (
                struct.unpack_from("<BBBBHHIH", group, 6 + index * 14)
            )
            dimensions = (width or 256, height or 256)
            if dimensions not in expected or dimensions in seen:
                raise ValueError(f"EXE 图标尺寸重复或与 ICO 不符：{dimensions}")
            actual = resource_bytes(resource_id, RT_ICON)
            if len(actual) != size or actual != expected[dimensions]:
                raise ValueError(f"EXE 的 {dimensions[0]}px 图像与源 ICO 不一致。")
            seen.add(dimensions)
        if seen != set(expected):
            raise ValueError("EXE 没有包含源 ICO 的全部七种尺寸。")
    finally:
        kernel.FreeLibrary(module)


def main() -> None:
    """解析本机路径并执行只读校验；任何失败均返回非零退出码。"""
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--exe", type=Path, default=Path("dist/batch-send-email.exe"), help="待核对 EXE 路径"
    )
    options = parser.parse_args()
    executable = options.exe
    if not executable.is_absolute():
        executable = ROOT / executable
    try:
        expected = ico_images(ROOT / "src-tauri" / "icons" / "icon.ico")
        check_executable(executable.resolve(), expected)
    except (OSError, ValueError) as error:
        raise SystemExit(f"EXE 图标检查失败：{error}") from error
    print("EXE 图标检查通过：资源组 32512 的七种尺寸与源 ICO 图像逐字节一致。")


if __name__ == "__main__":
    main()
