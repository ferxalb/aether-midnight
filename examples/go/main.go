// Minimal Go MCP stdio client; it calls the same Rust process as the Node/Bun example.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
)

type response struct {
	ID     int             `json:"id"`
	Result json.RawMessage `json:"result"`
	Error  *struct {
		Code    int    `json:"code"`
		Message string `json:"message"`
	} `json:"error"`
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	binary := os.Getenv("AETHER_MIDNIGHT_BIN")
	if binary == "" {
		binary = "aether-midnight"
	}
	repo := os.Getenv("AETHER_REPO")
	if repo == "" {
		return fmt.Errorf("set AETHER_REPO to a local repository path")
	}
	query := os.Getenv("AETHER_QUERY")
	if query == "" {
		query = "benchmark_target_00000"
	}

	args := []string{"mcp"}
	if database := os.Getenv("AETHER_MIDNIGHT_DB"); database != "" {
		args = []string{"--db", database, "mcp"}
	}
	cmd := exec.Command(binary, args...)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		return err
	}
	reader := bufio.NewReader(stdout)
	if _, err := call(stdin, reader, 1, "initialize", map[string]any{
		"protocolVersion": "2025-11-25",
		"capabilities":    map[string]any{},
		"clientInfo":      map[string]string{"name": "midnight-go-example", "version": "1.0.0"},
	}); err != nil {
		_ = cmd.Process.Kill()
		return err
	}
	if err := write(stdin, map[string]any{"jsonrpc": "2.0", "method": "notifications/initialized"}); err != nil {
		return err
	}
	result, err := call(stdin, reader, 2, "tools/call", map[string]any{
		"name":      "search_code",
		"arguments": map[string]any{"repo_path": repo, "query": query, "limit": 5},
	})
	if err != nil {
		_ = cmd.Process.Kill()
		return err
	}
	var pretty any
	if err := json.Unmarshal(result, &pretty); err != nil {
		return err
	}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(pretty); err != nil {
		return err
	}
	_ = stdin.Close()
	return cmd.Wait()
}

func call(stdin io.Writer, reader *bufio.Reader, id int, method string, params any) (json.RawMessage, error) {
	if err := write(stdin, map[string]any{"jsonrpc": "2.0", "id": id, "method": method, "params": params}); err != nil {
		return nil, err
	}
	line, err := reader.ReadBytes('\n')
	if err != nil {
		return nil, err
	}
	var res response
	if err := json.Unmarshal(line, &res); err != nil {
		return nil, err
	}
	if res.ID != id {
		return nil, fmt.Errorf("unexpected response id %d, wanted %d", res.ID, id)
	}
	if res.Error != nil {
		return nil, fmt.Errorf("MCP error %d: %s", res.Error.Code, res.Error.Message)
	}
	return res.Result, nil
}

func write(w io.Writer, value any) error {
	encoder := json.NewEncoder(w)
	return encoder.Encode(value)
}
