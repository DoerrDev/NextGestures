import type { Rect } from './api'
export interface XY { x: number; y: number }
export type MarkKind = 'rectangle' | 'arrow' | 'pen' | 'mosaic' | 'text'
export interface Mark { kind: MarkKind; points: XY[]; color: string; width: number; text?: string }

export function clampPoint(p: XY, width: number, height: number): XY {
  return { x: Math.max(0, Math.min(width, p.x)), y: Math.max(0, Math.min(height, p.y)) }
}
export function rectangle(a: XY, b: XY): Rect {
  return { x: Math.floor(Math.min(a.x, b.x)), y: Math.floor(Math.min(a.y, b.y)), width: Math.round(Math.abs(a.x - b.x)), height: Math.round(Math.abs(a.y - b.y)) }
}
export function physicalPoint(client: XY, viewport: { width: number; height: number }, pixels: { width: number; height: number }): XY {
  return clampPoint({ x: client.x * pixels.width / Math.max(1, viewport.width), y: client.y * pixels.height / Math.max(1, viewport.height) }, pixels.width, pixels.height)
}
export function paint(ctx: CanvasRenderingContext2D, marks: Mark[]) {
  for (const mark of marks) {
    if (!mark.points.length) continue
    const a = mark.points[0], b = mark.points[mark.points.length - 1]
    ctx.save()
    ctx.strokeStyle = mark.color; ctx.fillStyle = mark.color; ctx.lineWidth = mark.width
    ctx.lineCap = 'round'; ctx.lineJoin = 'round'
    if (mark.kind === 'rectangle') ctx.strokeRect(a.x, a.y, b.x - a.x, b.y - a.y)
    if (mark.kind === 'arrow') {
      const angle = Math.atan2(b.y - a.y, b.x - a.x), head = Math.max(12, mark.width * 4)
      ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y)
      ctx.moveTo(b.x - head * Math.cos(angle - Math.PI / 6), b.y - head * Math.sin(angle - Math.PI / 6)); ctx.lineTo(b.x, b.y)
      ctx.lineTo(b.x - head * Math.cos(angle + Math.PI / 6), b.y - head * Math.sin(angle + Math.PI / 6)); ctx.stroke()
    }
    if (mark.kind === 'pen') {
      ctx.beginPath(); ctx.moveTo(a.x, a.y)
      for (const p of mark.points.slice(1)) ctx.lineTo(p.x, p.y)
      ctx.stroke()
    }
    if (mark.kind === 'text') {
      const size = Math.max(18, mark.width * 7)
      ctx.font = `${size}px "Microsoft YaHei", sans-serif`; ctx.textBaseline = 'top'
      for (const [i, line] of (mark.text ?? '').split('\n').entries()) ctx.fillText(line, a.x, a.y + i * size * 1.3)
    }
    if (mark.kind === 'mosaic') {
      const box = rectangle(a, b), width = Math.min(box.width, ctx.canvas.width - box.x), height = Math.min(box.height, ctx.canvas.height - box.y)
      if (width > 0 && height > 0) {
        const small = document.createElement('canvas'); small.width = Math.max(1, Math.ceil(width / 16)); small.height = Math.max(1, Math.ceil(height / 16))
        small.getContext('2d')!.drawImage(ctx.canvas, box.x, box.y, width, height, 0, 0, small.width, small.height)
        ctx.imageSmoothingEnabled = false; ctx.drawImage(small, box.x, box.y, width, height)
      }
    }
    ctx.restore()
  }
}
export function cropped(image: HTMLImageElement, selection: Rect, marks: Mark[], maxSide?: number): HTMLCanvasElement {
  const canvas = document.createElement('canvas'); canvas.width = selection.width; canvas.height = selection.height
  const ctx = canvas.getContext('2d')!
  ctx.drawImage(image, selection.x, selection.y, selection.width, selection.height, 0, 0, selection.width, selection.height)
  paint(ctx, marks)
  if (maxSide && Math.max(canvas.width, canvas.height) > maxSide) {
    const scale = maxSide / Math.max(canvas.width, canvas.height), smaller = document.createElement('canvas')
    smaller.width = Math.max(1, Math.round(canvas.width * scale)); smaller.height = Math.max(1, Math.round(canvas.height * scale))
    smaller.getContext('2d')!.drawImage(canvas, 0, 0, smaller.width, smaller.height)
    return smaller
  }
  return canvas
}
