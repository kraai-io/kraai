import type {
	AgentApi,
	AgentHandle,
	AgentInfo,
	ChatMessage,
	// ModelInfo,
} from "agent-ts-bindings";
import { useState } from "react";
import { Button } from "@/components/ui/button";

function App(): React.JSX.Element {
	const [result, setResult] = useState<number | null>(null);
	const [agentApiStatus, setAgentApiStatus] =
		useState<string>("Not initialized");
	const [agentApi, setAgentApi] = useState<AgentApi | null>(null);
	// const [models, setModels] = useState<ModelInfo[]>([]);
	const [agents, setAgents] = useState<AgentInfo[]>([]);
	const [currentAgent, setCurrentAgent] = useState<AgentHandle | null>(null);
	const [history, setHistory] = useState<ChatMessage[]>([]);
	const [error, setError] = useState<string | null>(null);

	const handleTestRust = () => {
		const value = window.api.plus100(42);
		setResult(value);
	};

	const handleCreateAgentApi = () => {
		try {
			setError(null);
			const api = window.api.createAgentApi();
			setAgentApi(api);
			setAgentApiStatus("AgentAPI created successfully!");
			console.log("AgentAPI instance:", api);
		} catch (err) {
			const errorMsg = err instanceof Error ? err.message : String(err);
			setError(errorMsg);
			setAgentApiStatus(`Error: ${errorMsg}`);
			console.error("Failed to create AgentAPI:", err);
		}
	};

	// TODO: Re-enable when listModels is implemented
	// const handleListModels = () => {
	// 	if (!agentApi) {
	// 		setError("AgentAPI not initialized");
	// 		return;
	// 	}
	// 	try {
	// 		setError(null);
	// 		const modelList = agentApi.listModels();
	// 		setModels(modelList);
	// 		console.log("Models:", modelList);
	// 	} catch (err) {
	// 		const errorMsg = err instanceof Error ? err.message : String(err);
	// 		setError(errorMsg);
	// 		console.error("Failed to list models:", err);
	// 	}
	// };

	const handleCreateAgent = () => {
		if (!agentApi) {
			setError("AgentAPI not initialized");
			return;
		}
		try {
			setError(null);
			const agent = agentApi.createAgent("You are a helpful assistant.");
			setCurrentAgent(agent);
			console.log("Created agent:", agent);
			// Refresh agent list
			const agentList = agentApi.listAgents();
			setAgents(agentList);
		} catch (err) {
			const errorMsg = err instanceof Error ? err.message : String(err);
			setError(errorMsg);
			console.error("Failed to create agent:", err);
		}
	};

	const handleListAgents = () => {
		if (!agentApi) {
			setError("AgentAPI not initialized");
			return;
		}
		try {
			setError(null);
			const agentList = agentApi.listAgents();
			setAgents(agentList);
			console.log("Agents:", agentList);
		} catch (err) {
			const errorMsg = err instanceof Error ? err.message : String(err);
			setError(errorMsg);
			console.error("Failed to list agents:", err);
		}
	};

	const handleGetHistory = () => {
		if (!agentApi || !currentAgent) {
			setError("AgentAPI or agent not initialized");
			return;
		}
		try {
			setError(null);
			const chatHistory = agentApi.getHistory(currentAgent.id);
			setHistory(chatHistory);
			console.log("History:", chatHistory);
		} catch (err) {
			const errorMsg = err instanceof Error ? err.message : String(err);
			setError(errorMsg);
			console.error("Failed to get history:", err);
		}
	};

	// HTTP Request Test
	const [httpResult, setHttpResult] = useState<string | null>(null);
	const [httpLoading, setHttpLoading] = useState(false);

	const handleTestHttp = async () => {
		setHttpLoading(true);
		setError(null);
		try {
			// Test with httpbin.org
			const result = await window.api.testHttpRequest(
				"https://example.com/",
			);
			setHttpResult(result);
			console.log("HTTP Result:", result);
		} catch (err) {
			const errorMsg = err instanceof Error ? err.message : String(err);
			setError(`HTTP Test Error: ${errorMsg}`);
			console.error("HTTP Test failed:", err);
		} finally {
			setHttpLoading(false);
		}
	};

	return (
		<div className="p-4 max-w-4xl">
			<h1 className="underline text-2xl font-bold mb-4">Agent Demo</h1>
			<p className="mb-4">Agent desktop application with Rust bindings</p>

			{error && (
				<div className="bg-red-100 border border-red-400 text-red-700 px-4 py-3 rounded mb-4">
					<strong>Error:</strong> {error}
				</div>
			)}

			<div className="space-y-4">
				<div className="p-4 border rounded">
					<h2 className="font-bold mb-2">Basic Test</h2>
					<Button onClick={handleTestRust}>Test Rust Binding (plus100)</Button>
					{result !== null && (
						<p className="mt-2 text-green-600">
							Rust says: 42 + 100 = {result}
						</p>
					)}
				</div>

				<div className="p-4 border rounded">
					<h2 className="font-bold mb-2">HTTP Test</h2>
					<Button
						onClick={handleTestHttp}
						variant="outline"
						disabled={httpLoading}
					>
						{httpLoading ? "Testing..." : "Test HTTP Request (Rust)"}
					</Button>
					{httpResult && (
						<div className="mt-2 p-2 rounded text-xs overflow-auto max-h-40">
							<pre>{httpResult}</pre>
						</div>
					)}
				</div>

				<div className="p-4 border rounded">
					<h2 className="font-bold mb-2">AgentAPI</h2>
					<Button
						onClick={handleCreateAgentApi}
						variant="outline"
						className="mb-2"
					>
						Create AgentAPI Instance
					</Button>
					<p className="text-blue-600">{agentApiStatus}</p>
				</div>

				{agentApi && (
					<div className="p-4 border rounded">
						<h2 className="font-bold mb-2">Agent Management</h2>
						<div className="space-x-2">
							{/* TODO: Re-enable when listModels is implemented
							<Button onClick={handleListModels} variant="outline">
								List Models
							</Button>
							*/}
							<Button onClick={handleCreateAgent} variant="outline">
								Create Agent
							</Button>
							<Button onClick={handleListAgents} variant="outline">
								List Agents
							</Button>
							{currentAgent && (
								<Button onClick={handleGetHistory} variant="outline">
									Get History
								</Button>
							)}
						</div>

						{/* TODO: Re-enable when listModels is implemented
						{models.length > 0 && (
							<div className="mt-4">
								<h3 className="font-semibold">Available Models:</h3>
								<ul className="list-disc pl-5">
									{models.map((model) => (
										<li key={model.id}>
											{model.name} ({model.id})
										</li>
									))}
								</ul>
							</div>
						)}
						*/}

						{agents.length > 0 && (
							<div className="mt-4">
								<h3 className="font-semibold">Agents:</h3>
								<ul className="list-disc pl-5">
									{agents.map((agent) => (
										<li key={agent.id}>
											{agent.id} - {agent.messageCount} messages
										</li>
									))}
								</ul>
							</div>
						)}

						{history.length > 0 && (
							<div className="mt-4">
								<h3 className="font-semibold">Chat History:</h3>
								<div className="space-y-2">
									{history.map((msg, idx) => (
										<div key={idx} className="p-2 bg-gray-100 rounded">
											<strong>{ChatRole[msg.role]}:</strong> {msg.content}
										</div>
									))}
								</div>
							</div>
						)}
					</div>
				)}
			</div>
		</div>
	);
}

// Helper to convert ChatRole enum to string
function ChatRole(role: number): string {
	switch (role) {
		case 0:
			return "System";
		case 1:
			return "User";
		case 2:
			return "Assistant";
		default:
			return "Unknown";
	}
}

export default App;
