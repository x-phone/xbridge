import { useState, useEffect, useMemo } from 'react'
import type { CallData, Message, EventEntry } from '../App'
import Conversation from './Conversation'
import EventLog from './EventLog'
import CallList from './CallList'

type Props = {
  connected: boolean
  mode: 'ai' | 'ivr'
  calls: Record<string, CallData>
  selectedCall: CallData | null
  selectedCallId: string | null
  globalEvents: EventEntry[]
  onSelectCall: (callId: string) => void
  onHangup: (callId: string) => void
  onDisconnect: () => void
}

export default function Dashboard({
  connected,
  mode,
  calls,
  selectedCall,
  selectedCallId,
  globalEvents,
  onSelectCall,
  onHangup,
  onDisconnect,
}: Props) {
  // Merge global events + selected call events, sorted by time
  const events = useMemo(() => {
    const callEvents = selectedCall?.events ?? []
    return [...globalEvents, ...callEvents].sort((a, b) => a.time.localeCompare(b.time))
  }, [globalEvents, selectedCall?.events])

  const hasAnyCalls = Object.keys(calls).length > 0

  return (
    <div className="h-screen flex flex-col overflow-hidden">
      {/* Header */}
      <header className="bg-slate-900 border-b border-slate-800 px-6 py-3 flex items-center justify-between shrink-0">
        <div className="flex items-center gap-3">
          <h1 className="text-lg font-semibold text-white">xbridge Voice AI Demo</h1>
          <span className="text-xs bg-slate-800 text-slate-400 px-2 py-0.5 rounded-full">
            {mode === 'ai' ? 'Voice AI' : 'IVR'}
          </span>
        </div>
        <div className="flex items-center gap-4">
          <span className="flex items-center gap-2 text-sm">
            <span
              className={`w-2 h-2 rounded-full ${connected ? 'bg-emerald-400' : 'bg-red-400'}`}
            />
            <span className="text-slate-400">
              {connected ? 'Connected' : 'Disconnected'}
            </span>
          </span>
          <button
            onClick={onDisconnect}
            className="text-slate-500 hover:text-slate-300 text-sm transition-colors cursor-pointer"
            title="Back to setup"
          >
            Settings
          </button>
        </div>
      </header>

      {/* Body: call list + main content + event log */}
      <div className="flex-1 flex min-h-0">
        {/* Call list sidebar (left) */}
        <CallList
          calls={calls}
          selectedCallId={selectedCallId}
          onSelectCall={onSelectCall}
        />

        {/* Main content */}
        <main className="flex-1 flex flex-col min-h-0 bg-slate-950">
          {selectedCall && selectedCall.status === 'active' ? (
            <CallView call={selectedCall} onHangup={onHangup} />
          ) : selectedCall ? (
            <EndedCallView call={selectedCall} />
          ) : (
            <WaitingView mode={mode} />
          )}
        </main>

        {/* Event log sidebar (right) */}
        <EventLog events={events} />
      </div>
    </div>
  )
}

function WaitingView({ mode }: { mode: string }) {
  return (
    <div className="flex-1 flex items-center justify-center text-center p-6">
      <div>
        <div className="text-5xl mb-4 opacity-20 select-none">
          {mode === 'ivr' ? '\u260E' : '\u{1F399}'}
        </div>
        <h2 className="text-xl text-slate-400 mb-3">Waiting for calls...</h2>
        <div className="text-sm text-slate-500 space-y-1">
          <p>
            Register a softphone as extension{' '}
            <code className="text-slate-300 bg-slate-800 px-1.5 py-0.5 rounded">1001</code>
          </p>
          <p>
            Password:{' '}
            <code className="text-slate-300 bg-slate-800 px-1.5 py-0.5 rounded">
              password123
            </code>
          </p>
          <p>
            Dial{' '}
            <code className="text-slate-300 bg-slate-800 px-1.5 py-0.5 rounded text-lg">
              2000
            </code>
          </p>
        </div>
      </div>
    </div>
  )
}

function CallView({
  call,
  onHangup,
}: {
  call: CallData
  onHangup: (id: string) => void
}) {
  const [elapsed, setElapsed] = useState(0)

  useEffect(() => {
    const iv = setInterval(
      () => setElapsed(Math.floor((Date.now() - call.startTime) / 1000)),
      1000,
    )
    return () => clearInterval(iv)
  }, [call.startTime])

  const mm = String(Math.floor(elapsed / 60)).padStart(2, '0')
  const ss = String(elapsed % 60).padStart(2, '0')

  return (
    <div className="flex-1 flex flex-col min-h-0">
      {/* Call bar */}
      <div className="flex items-center justify-between px-6 py-3 bg-slate-900/50 border-b border-slate-800 shrink-0">
        <div className="flex items-center gap-3">
          <span className="w-2 h-2 bg-emerald-400 rounded-full animate-pulse" />
          <span className="text-sm text-slate-300">
            {call.from} &rarr; {call.to}
          </span>
        </div>
        <div className="flex items-center gap-4">
          <span className="text-sm text-slate-500 font-mono">
            {mm}:{ss}
          </span>
          <button
            onClick={() => onHangup(call.callId)}
            className="bg-red-600 hover:bg-red-500 text-white text-sm px-4 py-1.5 rounded-lg transition-colors cursor-pointer"
          >
            End Call
          </button>
        </div>
      </div>

      <Conversation messages={call.messages} />
    </div>
  )
}

function EndedCallView({ call }: { call: CallData }) {
  return (
    <div className="flex-1 flex flex-col min-h-0">
      {/* Call bar (ended) */}
      <div className="flex items-center justify-between px-6 py-3 bg-slate-900/50 border-b border-slate-800 shrink-0">
        <div className="flex items-center gap-3">
          <span className="w-2 h-2 bg-slate-600 rounded-full" />
          <span className="text-sm text-slate-500">
            {call.from} &rarr; {call.to}
          </span>
        </div>
        <span className="text-xs text-slate-600">Ended</span>
      </div>

      <Conversation messages={call.messages} />
    </div>
  )
}
