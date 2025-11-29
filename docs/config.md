# config.toml

# Providers are the primary configuration entities
[[providers]]
id = "openai_main"
type = "openai"
api_key_env = "OPENAI_API_KEY" # Use env var for security
url = ""

[[providers]]
id = "mistral_local"
type = "mistral_rs"
# No other config needed here, models are defined below

# Explicitly define local models this provider is responsible for
[[models]]
id = "phi-3-mini-local"
name = "Phi-3 Mini (Local)"
max_context = 4096
provider_id = "mistral_local" # Link to the provider
type = "local"
repo_id = "TheBloke/Phi-3-mini-4k-instruct-GGUF"
filename = "phi-3-mini-4k-instruct.Q4_K_M.gguf"

# Optional: Create an alias or override for a remote model
[[models]]
id = "gpt-4o-alias"
name = "GPT-4o (My Alias)"
provider_id = "openai_main"
type = "remote_alias"
# The actual ID to use when calling the API
api_model_id = "gpt-4o" 
# Override the context size if the API doesn't provide it
max_context = 128000
