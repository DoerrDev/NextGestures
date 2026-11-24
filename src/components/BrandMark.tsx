/** 应用图标（与 src-tauri/icons/icon.svg 同一图形），用于顶栏品牌位 */
export default function BrandMark({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 100 100" aria-hidden="true">
      <rect x="4" y="4" width="92" height="92" rx="22" fill="#F0EEE9" />
      <rect
        x="4.5"
        y="4.5"
        width="91"
        height="91"
        rx="21.5"
        fill="none"
        stroke="#1C1C1E"
        strokeOpacity=".12"
      />
      <path
        d="M31 71 V31 L69 69 V40"
        fill="none"
        stroke="#1C1C1E"
        strokeWidth="8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M59.5 42 L69 29.5 L78.5 42 Z"
        fill="#1C1C1E"
        stroke="#1C1C1E"
        strokeWidth="3"
        strokeLinejoin="round"
      />
      <circle cx="31" cy="72" r="7" fill="#E4572E" />
    </svg>
  )
}
