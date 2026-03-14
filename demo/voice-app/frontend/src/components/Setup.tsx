import { useState, type FormEvent } from 'react'

type Props = {
  onConnect: (cfg: {
    mode: 'ai' | 'ivr'
    deepgramKey?: string
    systemPrompt?: string
  }) => void
}

export default function Setup({ onConnect }: Props) {
  const [key, setKey] = useState(() => localStorage.getItem('dg_key') || '')
  const [prompt, setPrompt] = useState(
    'You are a friendly AI receptionist for Acme Corp. Keep responses brief and helpful.',
  )

  const connectAI = (e: FormEvent) => {
    e.preventDefault()
    if (!key.trim()) return
    localStorage.setItem('dg_key', key)
    onConnect({ mode: 'ai', deepgramKey: key, systemPrompt: prompt })
  }

  return (
    <div className="min-h-screen bg-slate-950 flex items-center justify-center p-4">
      <div className="bg-slate-900 rounded-2xl shadow-2xl p-8 max-w-lg w-full border border-slate-800">
        <h1 className="text-2xl font-bold text-white text-center mb-1">
          xbridge Voice AI Demo
        </h1>
        <p className="text-sm text-slate-500 text-center mb-8">
          Real-time voice AI powered by xbridge
        </p>

        <form onSubmit={connectAI} className="space-y-5">
          <div>
            <label className="block text-sm font-medium text-slate-400 mb-1.5">
              Deepgram API Key
            </label>
            <input
              type="password"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder="Enter your Deepgram API key"
              className="w-full bg-slate-800 text-white rounded-lg px-4 py-2.5
                         border border-slate-700 placeholder-slate-600
                         focus:border-blue-500 focus:outline-none focus:ring-1 focus:ring-blue-500"
            />
          </div>

          <div>
            <label className="block text-sm font-medium text-slate-400 mb-1.5">
              System Prompt
            </label>
            <textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              rows={3}
              className="w-full bg-slate-800 text-white rounded-lg px-4 py-2.5
                         border border-slate-700 placeholder-slate-600
                         focus:border-blue-500 focus:outline-none focus:ring-1 focus:ring-blue-500
                         resize-none"
            />
          </div>

          <button
            type="submit"
            className="w-full bg-blue-600 hover:bg-blue-500 text-white font-medium
                       py-2.5 rounded-lg transition-colors"
          >
            Connect with Voice AI
          </button>
        </form>

        <div className="relative my-6">
          <div className="absolute inset-0 flex items-center">
            <div className="w-full border-t border-slate-800" />
          </div>
          <div className="relative flex justify-center">
            <span className="bg-slate-900 px-3 text-xs text-slate-600">or</span>
          </div>
        </div>

        <button
          onClick={() => onConnect({ mode: 'ivr' })}
          className="w-full bg-slate-800 hover:bg-slate-700 text-slate-300 font-medium
                     py-2.5 rounded-lg transition-colors border border-slate-700"
        >
          Try IVR Demo (no API key needed)
        </button>

        <p className="text-xs text-slate-600 mt-6 text-center">
          API keys are stored in your browser only and sent per-session.
        </p>
      </div>
    </div>
  )
}
