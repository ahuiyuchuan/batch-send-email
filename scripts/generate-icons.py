"""从用户提供的原图重建所有应用图标。@author ahui

默认读取 assets/app-icon-source.png，生成前端 PNG 和 Tauri PNG/多尺寸 ICO。
依赖当前 Python 环境中的 Pillow 9.1 或更新版本；不联网、不安装软件。

这份源图的灰白棋盘格已经写入 RGB 像素，并非透明通道。脚本仅移除与画布
外边缘连通的中性背景；蓝色轮廓包围的白色人物、信封、渐变和阴影均保留。
若以后换成带真实透明通道的源图，则直接使用原透明度，不再执行背景分离。

执行 python scripts/generate-icons.py 可重建；加 --check 仅检查现有产物。
"""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

try:
    from PIL import Image, ImageChops, ImageDraw
except ImportError as error:
    raise SystemExit(
        "生成图标需要当前 Python 环境已有 Pillow；运行 EXE 不需要 Python。"
    ) from error


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "app-icon-source.png"
FRONTEND_ICON = ROOT / "frontend" / "assets" / "app-icon.png"
TAURI_ICONS = ROOT / "src-tauri" / "icons"
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
PNG_SIZES = {
    "icon.png": 512,
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
}


def prepare_icon(source: Path) -> Image.Image:
    """保留标识内部像素，去除外部背景并居中到有少量透明边距的正方形。

    Args:
        source: 用户提供的 PNG 原图，不会被修改或覆盖。

    Returns:
        512×512 RGBA 图像；背景透明，源标识按原比例缩放。

    Raises:
        ValueError: 不透明源图没有预期蓝色主体，或主体与画布边缘接触，无法可靠分离。
        OSError: 原图缺失、损坏或无法读取。
    """
    with Image.open(source) as original:
        image = original.convert("RGBA")
    alpha = image.getchannel("A")
    if alpha.getextrema()[0] == 255:
        # 已核对的原图：外围棋盘格近于中性灰白，蓝色主体的 B-R 差值明显更大。
        # 先以蓝色轮廓作屏障，再从外缘连通填充，避免按“白色”全局抠除内部图形。
        red, _green, blue, _alpha = image.split()
        barrier = ImageChops.subtract(blue, red).point(
            lambda value: 255 if value >= 12 else 0
        )
        bbox = barrier.getbbox()
        if (
            not bbox
            or bbox[0] == 0
            or bbox[1] == 0
            or bbox[2] >= image.width
            or bbox[3] >= image.height
        ):
            raise ValueError("原图没有可安全分离的蓝色主体，请提供带透明背景的原图。")
        ImageDraw.floodfill(barrier, (0, 0), 128)
        alpha = barrier.point(lambda value: 0 if value == 128 else 255)
        if (
            not alpha.getbbox()
            or alpha.getpixel((image.width // 2, image.height // 2)) != 255
        ):
            raise ValueError("主体轮廓没有闭合，不能自动移除背景。")
        image.putalpha(alpha)

    bbox = alpha.getbbox()
    if not bbox:
        raise ValueError("源图完全透明，没有可生成的应用标识。")
    cropped = image.crop(bbox)
    edge = max(cropped.size)
    # 各边约 2.5% 留白使任务栏和页面图标不会贴边；保持原图比例，不拉伸标识。
    padding = max(1, round(edge * 0.025))
    canvas = Image.new(
        "RGBA", (edge + padding * 2, edge + padding * 2), (0, 0, 0, 0)
    )
    canvas.alpha_composite(
        cropped,
        ((canvas.width - cropped.width) // 2, (canvas.height - cropped.height) // 2),
    )
    return canvas.resize((512, 512), Image.Resampling.LANCZOS)


def generate() -> None:
    """从保留的源图覆盖生成受控资产；不修改源图、界面代码或用户文件。"""
    icon = prepare_icon(SOURCE)
    FRONTEND_ICON.parent.mkdir(parents=True, exist_ok=True)
    TAURI_ICONS.mkdir(parents=True, exist_ok=True)
    icon.save(FRONTEND_ICON, format="PNG", optimize=True)
    for filename, size in PNG_SIZES.items():
        icon.resize((size, size), Image.Resampling.LANCZOS).save(
            TAURI_ICONS / filename, format="PNG", optimize=True
        )
    icon.save(
        TAURI_ICONS / "icon.ico",
        format="ICO",
        sizes=[(size, size) for size in ICO_SIZES],
    )


def check() -> None:
    """只读验证尺寸与真实透明度，确保 ICO 包含 Windows 常用的七种图层。"""
    expected = {
        FRONTEND_ICON: 512,
        **{TAURI_ICONS / name: size for name, size in PNG_SIZES.items()},
    }
    for path, size in expected.items():
        with Image.open(path) as image:
            if image.size != (size, size) or image.mode != "RGBA":
                raise ValueError(f"图标尺寸或通道不符：{path.relative_to(ROOT)}")
            alpha = image.getchannel("A")
            if alpha.getextrema() != (0, 255) or image.getpixel((0, 0))[3] != 0:
                raise ValueError(f"图标背景不透明：{path.relative_to(ROOT)}")
    with Image.open(TAURI_ICONS / "icon.ico") as icon:
        if icon.ico.sizes() != {(size, size) for size in ICO_SIZES}:
            raise ValueError("ICO 缺少预期尺寸。")
    print(
        "图标检查通过：前端/Tauri PNG 为透明 RGBA，"
        "ICO 包含 16/24/32/48/64/128/256。"
    )


def main() -> None:
    """按命令参数生成或只读检查；失败由 Python 返回非零退出码，不能静默使用旧图标。"""
    # Windows 终端的默认代码页可能与调用方不同；明确输出 UTF-8，避免验收日志乱码。
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="只检查现有图标，不重新生成")
    options = parser.parse_args()
    if not options.check:
        generate()
    check()


if __name__ == "__main__":
    main()
