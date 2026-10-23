# Eclipse／日蚀图标

当前产品名为 **uu-client**，二阶段产品方向为 **Eclipse／日蚀**。
图标以杏色太阳与天蓝月面相叠，表达采集、串流和远程桌面的连接；浅薄荷背景保持明亮、清新的视觉。

- `eclipse-master.png`：生成原图。
- `eclipse-256.png`：窗口、任务栏和应用界面使用。
- `eclipse-512.png`：Linux 桌面图标。
- `eclipse.ico`：Windows EXE 图标，包含 16、24、32、48、64、128、256 像素规格。

图片使用内置 imagegen 工具生成；PNG 缩放与 ICO 封装使用 ImageMagick，构建时直接使用已提交的资源，无需额外图像工具。

最终生成提示词：

```text
Use case: logo-brand. Create a single desktop application icon, square full-bleed composition, entirely opaque. The entire image background must be one continuous very pale mint color to all four edges and all four corners, with no surrounding area. Center a fresh modern stylized solar eclipse: a large clean sky-blue / light turquoise circular moon partially covers a soft warm apricot sun disk offset to its upper left, exposing a broad graceful crescent, subtle soft warm corona between them. Use simple crisp circles, smooth restrained gradients, modern flat vector-like finish, minimal soft shading. One bold memorable geometric eclipse mark occupying 70% of the square, with generous balanced padding. Airy light palette, calm optimistic feeling, legible as a desktop icon at 32 px. No text, letters, stars, dust, noise, splashes, vignette, dark background, black areas, transparency, mockup, tile border or rounded-square silhouette. Background fills the entire square without any cutout. This is the original Eclipse / 日蚀 brand symbol for a remote desktop application currently named uu-client.
```

在本目录重新导出运行资源：

```sh
convert eclipse-master.png -resize 256x256 eclipse-256.png
convert eclipse-master.png -resize 512x512 eclipse-512.png
convert eclipse-master.png -define icon:auto-resize=256,128,64,48,32,24,16 eclipse.ico
```

## UU 兼容入口标志

`uu-remote.ico` 为网易 UU 远程官网的原始 favicon，来源 https://uuyc.163.com/favicon.ico，仅用于识别可选的 UU 兼容入口。相关商标和图像权利归网易所有，不属于本项目的双许可，也不代表官方合作或背书。
