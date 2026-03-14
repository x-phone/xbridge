import { useState, useEffect, useMemo } from 'react'
import type { CallData } from '../App'

type Props = {
  calls: Record<string, CallData>
  selectedCallId: string | null
  onSelectCall: (callId: string) => void
}

export default function CallList({ calls, selectedCallId, onSelectCall }: Props) {
  // Sort calls: active first, then by startTime descending (newest first)
  const sortedCalls = useMemo(() => {
    return Object.values(calls).sort((a, b) => {
      if (a.status !== b.status) return a.status === 'active' ? -1 : 1
      return b.startTime - a.startTime
    })
  }, [calls])

  if (sortedCalls.length === 0) {
    return (
      <aside className="w-56 shrink-0 bg-slate-900 border-r border-slate-800 flex flex-col min-h-0">
        <div className="px-4 py-3 border-b border-slate-800 shrink-0">
          <span className="text-[11px] text-slate-500 font-medium uppercase tracking-wider">
            Calls
          </span>
        </div>
        <div className="flex-1 flex items-center justify-center">
          <span className="text-xs text-slate-700">No calls yet</span>
        </div>
      </aside>
    )
  }

  return (
    <aside className="w-56 shrink-0 bg-slate-900 border-r border-slate-800 flex flex-col min-h-0">
      <div className="px-4 py-3 border-b border-slate-800 shrink-0">
        <span className="text-[11px] text-slate-500 font-medium uppercase tracking-wider">
          Calls ({sortedCalls.length})
        </span>
      </div>
      <div className="flex-1 overflow-y-auto">
        {sortedCalls.map((call) => (
          <CallItem
            key={call.callId}
            call={call}
            selected={call.callId === selectedCallId}
            onClick={() => onSelectCall(call.callId)}
          />
        ))}
      </div>
    </aside>
  )
}

function CallItem({
  call,
  selected,
  onClick,
}: {
  call: CallData
  selected: boolean
  onClick: () => void
}) {
  const isActive = call.status === 'active'

  return (
    <button
      onClick={onClick}
      className={`w-full text-left px-4 py-3 border-b border-slate-800/50 transition-colors cursor-pointer ${
        selected
          ? 'bg-slate-800'
          : 'hover:bg-slate-800/50'
      }`}
    >
      <div className="flex items-center justify-between mb-1">
        <div className="flex items-center gap-2">
          <span
            className={`w-1.5 h-1.5 rounded-full ${
              isActive ? 'bg-emerald-400 animate-pulse' : 'bg-slate-600'
            }`}
          />
          <span className={`text-xs font-medium ${isActive ? 'text-slate-200' : 'text-slate-500'}`}>
            {call.from}
          </span>
        </div>
        {isActive && <CallTimer startTime={call.startTime} />}
      </div>
      <div className="text-[11px] text-slate-600 pl-3.5">
        &rarr; {call.to}
        {!isActive && call.reason && (
          <span className="ml-1.5 text-slate-700">({call.reason})</span>
        )}
      </div>
    </button>
  )
}

function CallTimer({ startTime }: { startTime: number }) {
  const [elapsed, setElapsed] = useState(0)

  useEffect(() => {
    const iv = setInterval(
      () => setElapsed(Math.floor((Date.now() - startTime) / 1000)),
      1000,
    )
    return () => clearInterval(iv)
  }, [startTime])

  const mm = String(Math.floor(elapsed / 60)).padStart(2, '0')
  const ss = String(elapsed % 60).padStart(2, '0')

  return (
    <span className="text-[10px] text-slate-600 font-mono">
      {mm}:{ss}
    </span>
  )
}
