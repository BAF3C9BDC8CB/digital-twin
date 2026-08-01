with open('config/pipeline.yaml', 'r') as f:
    content = f.read()
content = content.replace('embed: true', 'embed: false')
content = content.replace('llm_provider: siliconflow', 'llm_provider: xinference')
with open('config/pipeline.yaml', 'w') as f:
    f.write(content)
print("Done: embed=false, llm_provider=xinference")
