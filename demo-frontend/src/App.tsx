import { useState, useEffect } from 'react';
import './App.css';

function App() {
  const [isDarkMode] = useState(true);

  useEffect(() => {
    if (isDarkMode) {
      document.body.classList.add('dark-mode');
    } else {
      document.body.classList.remove('dark-mode');
    }
  }, [isDarkMode]);

  const [client, setClient] = useState('open-webui');
  const [model, setModel] = useState('gemma3:4b');
  const [systemPrompt, setSystemPrompt] = useState('You are a helpful AI assistant.');
  const [userMessage, setUserMessage] = useState('Hello, how are you?');
  const [response, setResponse] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    setResponse('');

    try {
      const res = await fetch('http://localhost:8080/api/generate-stream', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          client,
          model,
          system_prompt: systemPrompt,
          user_message: userMessage,
        }),
      });

      if (!res.ok) {
        throw new Error(`HTTP error! status: ${res.status}`);
      }

      const reader = res.body?.getReader();
      if (!reader) {
        throw new Error('Failed to get reader from response body.');
      }

      const decoder = new TextDecoder();
      let accumulatedResponse = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) {
          break;
        }
        const chunk = decoder.decode(value, { stream: true });
        // SSE events are prefixed with "data: " and end with "\\n\\n"
        // We need to parse each event
        chunk.split('\n').forEach(eventString => {
          if (eventString.startsWith('data: ')) {
            const data = eventString.substring(6);
            if (data === '[DONE]') {
              // This is the end of the stream
              return;
            }
            accumulatedResponse += data;
            setResponse(accumulatedResponse);
          } else if (eventString.startsWith('event: complete')) {
            // This is the custom 'complete' event from the backend
            return;
          } else if (eventString.startsWith('event: error')) {
            const errorData = eventString.substring(eventString.indexOf('data: ') + 6);
            setError(`Stream error: ${errorData}`);
            return;
          }
        });
      }
    } catch (err: any) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="App">
      <h1>LLM Interaction Frontend</h1>
      <div className="main-content">
        <form onSubmit={handleSubmit}>
          <div className="form-group">
            <label htmlFor="client">Client ID:</label>
            <input
              type="text"
              id="client"
              value={client}
              onChange={(e) => setClient(e.target.value)}
              required
            />
          </div>
          <div className="form-group">
            <label htmlFor="model">Model ID:</label>
            <input
              type="text"
              id="model"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              required
            />
          </div>
          <div className="form-group">
            <label htmlFor="systemPrompt">System Prompt:</label>
            <textarea
              id="systemPrompt"
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              rows={3}
              required
            ></textarea>
          </div>
          <div className="form-group">
            <label htmlFor="userMessage">User Message:</label>
            <textarea
              id="userMessage"
              value={userMessage}
              onChange={(e) => setUserMessage(e.target.value)}
              rows={5}
              required
            ></textarea>
          </div>
          <button type="submit" disabled={loading}>
            {loading ? 'Generating...' : 'Generate Response'}
          </button>
        </form>

        <div className="response-section">
          <h2>Response:</h2>
          {error && <p className="error">Error: {error}</p>}
          <div className="response-container">
            <p className="response-text">
              {response || 'Waiting for response...'}
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;
