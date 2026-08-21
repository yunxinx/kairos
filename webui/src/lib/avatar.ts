/**
 * 头像预处理：把任意尺寸的图片缩成固定边长的 PNG data URL。
 *
 * 原图直传会把 MB 级 base64 写进 `users.avatar`，而后端对 data URL 有长度上限
 * （见 `store::users::validate_avatar`），超限会被拒。这里在浏览器侧先降采样，
 * 既让上传必定落在上限内，也避免把原图分辨率带进数据库。
 */

/** 输出边长（正方形）。头像最大显示尺寸远小于此，256 足够清晰。 */
const AVATAR_EDGE = 256;

/** 允许的输入类型；与后端 data URL 允许名单对齐（SVG 除外，见后端注释）。 */
const ACCEPTED_TYPES = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'];

export function isAcceptedAvatarType(type: string): boolean {
  return ACCEPTED_TYPES.includes(type);
}

/**
 * 读取文件并居中裁切缩放成 `AVATAR_EDGE` 见方的 PNG data URL。
 *
 * 居中裁切而非拉伸：头像是圆形展示，拉伸会让人脸变形。
 */
export async function downscaleAvatar(file: File): Promise<string> {
  const bitmap = await loadBitmap(file);
  try {
    const canvas = document.createElement('canvas');
    canvas.width = AVATAR_EDGE;
    canvas.height = AVATAR_EDGE;
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      throw new Error('canvas 2d context unavailable');
    }
    // 取原图中间的正方形区域，再铺满整个画布。
    const side = Math.min(bitmap.width, bitmap.height);
    const sx = (bitmap.width - side) / 2;
    const sy = (bitmap.height - side) / 2;
    ctx.drawImage(bitmap, sx, sy, side, side, 0, 0, AVATAR_EDGE, AVATAR_EDGE);
    return canvas.toDataURL('image/png');
  } finally {
    if ('close' in bitmap) {
      bitmap.close();
    }
  }
}

/** 优先用 `createImageBitmap`；不支持时回退到 `<img>` + object URL。 */
async function loadBitmap(file: File): Promise<ImageBitmap | HTMLImageElement> {
  if (typeof createImageBitmap === 'function') {
    return createImageBitmap(file);
  }
  const url = URL.createObjectURL(file);
  try {
    return await new Promise<HTMLImageElement>((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve(img);
      img.onerror = () => reject(new Error('图片解码失败'));
      img.src = url;
    });
  } finally {
    URL.revokeObjectURL(url);
  }
}
