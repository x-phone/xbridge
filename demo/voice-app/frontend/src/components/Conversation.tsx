import { useRef, useEffect } from 'react'
import type { Message } from '../App'

export default function Conversation({ messages }: { messages: Message[] }) {
  const endRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  if (messages.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-slate-600 text-sm">
        Conversation will appear here...
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-y-auto p-6 space-y-3">
      {messages.map((msg) => (
        <div
          key={msg.id}
          className={`flex ${msg.role === 'caller' ? 'justify-end' : 'justify-start'}`}
        >
          <div
            className={`max-w-[80%] rounded-2xl px-4 py-2.5 ${
              msg.role === 'caller'
                ? 'bg-blue-600 text-white'
                : msg.role === 'ai'
                  ? 'bg-slate-800 text-slate-100'
                  : 'bg-emerald-900/40 text-emerald-200 border border-emerald-800/50'
            }`}
          >
            <div className="text-[11px] opacity-60 mb-0.5">
              {msg.role === 'caller' ? 'Caller' : msg.role === 'ai' ? 'AI' : 'System'}
            </div>
            <p className="text-sm leading-relaxed">
              {msg.text}
              {!msg.isFinal && (
                <span className="inline-block w-1.5 h-4 bg-current ml-0.5 animate-pulse align-text-bottom" />
              )}
            </p>
          </div>
        </div>
      ))}
      <div ref={endRef} />
    </div>
  )
}
