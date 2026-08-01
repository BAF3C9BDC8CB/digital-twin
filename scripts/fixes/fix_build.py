#!/usr/bin/env python3
with open('src/interfaces/cli/build.rs', 'r') as f:
    content = f.read()

# Fix url access - remove .as_ref() since url is already String
content = content.replace('.and_then(|c| c.url.as_ref())', '.map(|c| c.url.as_str())')
content = content.replace('.and_then(|c| c.model_llm.as_ref())', '.map(|c| c.model_llm.as_str())')

# Fix the unwrap_or to use &str directly
content = content.replace('.unwrap_or("http://localhost:9997/v1")', '.unwrap_or("http://localhost:9997/v1").to_string()')
content = content.replace('.unwrap_or("qwen3.5")', '.unwrap_or("qwen3.5").to_string()')

# Fix LlmClientProcessor::new call to include model
old_call = '''registry.register(Box::new(LlmClientProcessor::new(
                    infer_client.clone(),
                    Arc::new(prompts),
                    llm_config,
                )))'''

new_call = '''registry.register(Box::new(LlmClientProcessor::new(
                    infer_client.clone(),
                    infer_model.clone(),
                    Arc::new(prompts),
                    llm_config,
                )))'''

content = content.replace(old_call, new_call)

with open('src/interfaces/cli/build.rs', 'w') as f:
    f.write(content)

print('Fixed build.rs')
