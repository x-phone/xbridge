import { useState, useCallback, useRef, useEffect } from 'react'
import Setup from './components/Setup'
import Dashboard from './components/Dashboard'

export type Message = {
  id: number
  role: 'caller' | 'ai' | 'system'
  text: string
  isFinal: boolean
}

export type EventEntry = {
  time: string
  message: string
}

export type CallData = {
  callId: string
  from: string
  to: string
  startTime: number
  status: 'active' | 'ended'
  reason?: string
  messages: Message[]
  events: EventEntry[]
}

export default function App() {
  const wsRef = useRef<WebSocket | null>(null)
  const [connected, setConnected] = useState(false)
  const [configured, setConfigured] = useState(false)
  const [mode, setMode] = useState<'ai' | 'ivr'>('ai')
  const [calls, setCalls] = useState<Record<string, CallData>>({})
  const [selectedCallId, setSelectedCallId] = useState<string | null>(null)
  const [globalEvents, setGlobalEvents] = useState<EventEntry[]>([])

  const addCallEvent = useCallback((callId: string, time: string, message: string) => {
    setCalls((prev) => {
      const call = prev[callId]
      if (!call) return prev
      return { ...prev, [callId]: { ...call, events: [...call.events, { time, message }] } }
    })
  }, [])

  const handleMessage = useCallback((data: Record<string, unknown>) => {
    switch (data.type) {
      case 'status':
        setConnected(data.connected as boolean)
        break
      case 'configured':
        setConfigured(true)
        setMode((data.mode as 'ai' | 'ivr') || 'ai')
        break
      case 'event': {
        const entry = { time: data.time as string, message: data.message as string }
        const cid = data.call_id as string | undefined
        if (cid) {
          addCallEvent(cid, entry.time, entry.message)
        } else {
          setGlobalEvents((prev) => [...prev, entry])
        }
        break
      }
      case 'call.started': {
        const callId = data.call_id as string
        const now = Date.now()
        setCalls((prev) => ({
          ...prev,
          [callId]: {
            callId,
            from: data.from as string,
            to: data.to as string,
            startTime: now,
            status: 'active',
            messages: [],
            events: [],
          },
        }))
        setSelectedCallId(callId)
        break
      }
      case 'call.ended': {
        const callId = data.call_id as string
        setCalls((prev) => {
          const call = prev[callId]
          if (!call) return prev
          return {
            ...prev,
            [callId]: { ...call, status: 'ended', reason: data.reason as string },
          }
        })
        break
      }
      case 'transcript': {
        const callId = data.call_id as string
        const role = data.role as 'caller' | 'ai' | 'system'
        const text = data.text as string
        const isFinal = (data.is_final as boolean) ?? true

        setCalls((prev) => {
          const call = prev[callId]
          if (!call) return prev
          const msgs = [...call.messages]
          if (role === 'caller') {
            const last = msgs[msgs.length - 1]
            if (last?.role === 'caller' && !last.isFinal) {
              msgs[msgs.length - 1] = { ...last, text, isFinal }
              return { ...prev, [callId]: { ...call, messages: msgs } }
            }
          }
          if (text) {
            msgs.push({ id: Date.now() + Math.random(), role, text, isFinal })
          }
          return { ...prev, [callId]: { ...call, messages: msgs } }
        })
        break
      }
    }
  }, [addCallEvent])

  const handleConnect = useCallback(
    (cfg: { mode: 'ai' | 'ivr'; deepgramKey?: string; systemPrompt?: string }) => {
      const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
      const ws = new WebSocket(`${protocol}//${location.host}/app`)
      ws.onopen = () => {
        ws.send(
          JSON.stringify({
            type: 'configure',
            mode: cfg.mode,
            deepgram_key: cfg.deepgramKey,
            system_prompt: cfg.systemPrompt,
          }),
        )
      }
      ws.onmessage = (e) => handleMessage(JSON.parse(e.data))
      ws.onclose = () => {
        setConnected(false)
        setConfigured(false)
        wsRef.current = null
      }
      wsRef.current = ws
    },
    [handleMessage],
  )

  const handleHangup = useCallback((callId: string) => {
    wsRef.current?.send(JSON.stringify({ type: 'hangup', call_id: callId }))
  }, [])

  const handleDisconnect = useCallback(() => {
    wsRef.current?.close()
    setConnected(false)
    setConfigured(false)
    setCalls({})
    setSelectedCallId(null)
    setGlobalEvents([])
  }, [])

  useEffect(() => () => wsRef.current?.close(), [])

  if (!configured) {
    return <Setup onConnect={handleConnect} />
  }

  const selectedCall = selectedCallId ? calls[selectedCallId] : null

  return (
    <Dashboard
      connected={connected}
      mode={mode}
      calls={calls}
      selectedCall={selectedCall}
      selectedCallId={selectedCallId}
      globalEvents={globalEvents}
      onSelectCall={setSelectedCallId}
      onHangup={handleHangup}
      onDisconnect={handleDisconnect}
    />
  )
}
