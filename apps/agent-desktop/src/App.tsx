import { useState } from "react";
import { Button } from "@/components/ui/button";

function App(): React.JSX.Element {
	const [result, setResult] = useState<number | null>(null);
	const [agentApiStatus, setAgentApiStatus] =
		useState<string>("Not initialized");

	const handleTestRust = () => {
		const value = window.api.plus100(42);
		setResult(value);
	};

	const handleCreateAgentApi = () => {
		try {
			// Create an AgentAPI instance
			const agentApi = new window.api.AgentApi();
			setAgentApiStatus("AgentAPI created successfully!");
			console.log("AgentAPI instance:", agentApi);
		} catch (error) {
			setAgentApiStatus(`Error: ${error}`);
			console.error("Failed to create AgentAPI:", error);
		}
	};

	return (
		<div className="p-4">
			<h1 className="underline text-2xl font-bold mb-4">Agent Demo</h1>
			<p className="mb-4">Agent desktop application placeholder</p>

			<div className="space-y-4">
				<div>
					<Button onClick={handleTestRust}>Test Rust Binding (plus100)</Button>
					{result !== null && (
						<p className="mt-2 text-green-600">
							Rust says: 42 + 100 = {result}
						</p>
					)}
				</div>

				<div>
					<Button onClick={handleCreateAgentApi} variant="outline">
						Create AgentAPI Instance
					</Button>
					<p className="mt-2 text-blue-600">{agentApiStatus}</p>
				</div>
			</div>
		</div>
	);
}

export default App;
