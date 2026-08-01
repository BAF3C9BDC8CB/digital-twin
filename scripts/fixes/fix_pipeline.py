with open('config/pipeline.yaml', 'r') as f:
    content = f.read()
content = content.replace('embed_provider: xinference', 'embed_provider: siliconflow')
content = content.replace('rerank_provider: xinference', 'rerank_provider: siliconflow')
with open('config/pipeline.yaml', 'w') as f:
    f.write(content)
print("Fixed pipeline.yaml")
