<template>
  <div class="app-container">
    <header>
      <h1>{{ title }}</h1>
    </header>
    <main>
      <router-view />
    </main>
  </div>
</template>

<script>
export default {
  name: "App",
  data() {
    return {
      title: "My Application"
    };
  },
  methods: {
    greet() {
      return "Hello from Vue";
    }
  }
};
</script>

<style scoped>
.app-container {
  font-family: sans-serif;
}
header {
  background: #42b883;
  color: white;
  padding: 1rem;
}
</style>
