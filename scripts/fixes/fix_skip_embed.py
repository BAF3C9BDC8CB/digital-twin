with open('src/application/build/pipeline.rs', 'r') as f:
    content = f.read()

# Fix 1: process_documents function - add skip_embed check
old = '            // Embed chunks in batches if embed service is available.\n            if let (Some(embed_svc), Some(vector_repo)) = (&embed, &vector) {\n                if !chunks.is_empty() {'
new = '            // Embed chunks in batches if embed service is available and not skipped.\n            if let (Some(embed_svc), Some(vector_repo)) = (&embed, &vector) {\n                if !chunks.is_empty() && !self.skip_embed {'

if old in content:
    content = content.replace(old, new)
    print("Fixed process_documents")
else:
    print("Warning: process_documents pattern not found")

with open('src/application/build/pipeline.rs', 'w') as f:
    f.write(content)
