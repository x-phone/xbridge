import { useRef, useEffect, useCallback } from 'react'
import type { EventEntry } from '../App'

export default function EventLog({ events }: { events: EventEntry[] }) {
  const endRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [events])

  const handleCopy = useCallback(() => {
    const text = events.map((e) => `${e.time} ${e.message}`).join('\n')
    navigator.clipboard.writeText(text)
  }, [events])

  return (
    <aside className="w-80 shrink-0 bg-slate-900 border-l border-slate-800 flex flex-col min-h-0">
      <div className="px-4 py-3 flex items-center justify-between border-b border-slate-800 shrink-0">
        <span className="text-[11px] text-slate-500 font-medium uppercase tracking-wider">
          Event Log
        </span>
        {events.length > 0 && (
          <button
            onClick={handleCopy}
            className="text-[11px] text-slate-600 hover:text-slate-400 transition-colors cursor-pointer"
            title="Copy event log"
          >
            Copy
          </button>
        )}
      </div>
      <div className="flex-1 overflow-y-auto px-4 py-2 font-mono text-xs">
        {events.length === 0 && (
          <div className="text-slate-700 py-1">No events yet</div>
        )}
        {events.map((evt, i) => (
          <div key={i} className="text-slate-500 py-0.5">
            <span className="text-slate-600">{evt.time}</span> {evt.message}
          </div>
        ))}
        <div ref={endRef} />
      </div>
    </aside>
  )
}
