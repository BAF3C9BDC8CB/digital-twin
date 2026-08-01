import re

with open('src/main.rs', 'r') as f:
    content = f.read()

# Remove SiliconFlowConfig struct and its helper functions + XInferenceConfig + EmbedRouterConfig
# Find the block starting with SiliconFlowConfig and ending before HanlpConfig

start_marker = '/// SiliconFlow service configuration from config.yaml `services.siliconflow`.'
end_marker = '/// HanLP service configuration from config.yaml `services.hanlp`.'

start_idx = content.find(start_marker)
end_idx = content.find(end_marker)

if start_idx != -1 and end_idx != -1:
    # Remove the block including the newline before HanLP
    content = content[:start_idx] + content[end_idx:]
    print("Removed SiliconFlowConfig + XInferenceConfig + EmbedRouterConfig block")
else:
    print(f"Could not find markers: start={start_idx}, end={end_idx}")

print('SiliconFlowConfig in content:', 'SiliconFlowConfig' in content)
print('XInferenceConfig in content:', 'XInferenceConfig' in content)
print('EmbedRouterConfig in content:', 'EmbedRouterConfig' in content)
print('Length:', len(content))

with open('src/main.rs', 'w') as f:
    f.write(content)
