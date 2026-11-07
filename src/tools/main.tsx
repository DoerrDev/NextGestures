import { useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { screens, type ScreenSession } from './api'
import { CaptureEditor } from './CaptureEditor'
import { PinWindow } from './PinWindow'
import './tools.css'

function Tools() {
  const [session, setSession] = useState<ScreenSession | null>(null)
  const [error, setError] = useState('')
  useEffect(() => { void screens.session().then(setSession).catch(e => setError(String(e))) }, [])
  if (error) return <div className="screen-fatal" role="alert">{error}<button onClick={() => window.close()}>关闭</button></div>
  if (!session) return <div className="screen-loading">正在准备画面…</div>
  return session.kind === 'pin' ? <PinWindow session={session} /> : <CaptureEditor session={session} />
}
createRoot(document.getElementById('root')!).render(<Tools />)
