import { Button } from "@/components/ui/button";
import { useState } from "react";

function App(): React.JSX.Element {
	const [result, setResult] = useState<number | null>(null);

	const handleTestRust = () => {
		const value = window.api.plus100(42);
		setResult(value);
	};

	return (
		<div className="p-4">
			<h1 className="underline text-2xl font-bold mb-4">Agent Demo</h1>
			<p className="mb-4">Agent desktop application placeholder</p>
			<Button onClick={handleTestRust}>Test Rust Binding (plus100)</Button>
			{result !== null && (
				<p className="mt-4 text-green-600">
					Rust says: 42 + 100 = {result}
				</p>
			)}
		</div>
	);
}

export default App;
