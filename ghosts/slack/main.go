// REVENANT Ghost - Slack Bot
//
// Posts ephemeral messages containing context cards to Slack channels.
// Ephemeral messages are visible only to the target user and cannot
// be seen by anyone else. They also don't persist - Slack automatically
// clears them, making them ideal for ghost annotations.
//
// The bot listens on a Unix socket for messages from the daemon,
// identical to the VS Code extension protocol.

package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/slack-go/slack"
)

// ─── Types ─────────────────────────────────────────────────────────

type Config struct {
	BotToken   string `json:"bot_token"`
	UserID     string `json:"user_id"`
	ChannelID  string `json:"channel_id"`
	SocketPath string `json:"socket_path"`
}

type ContextCard struct {
	ID          string `json:"id"`
	ProjectDir  string `json:"project_dir"`
	ProjectName string `json:"project_name"`
	Summary     string `json:"summary"`
	NextStep    string `json:"next_step"`
	TTLSeconds  int    `json:"ttl_seconds"`
}

type DaemonMessage struct {
	Type string       `json:"type"`
	Card *ContextCard `json:"card,omitempty"`
}

// ─── Main ──────────────────────────────────────────────────────────

func main() {
	config, err := loadConfig()
	if err != nil {
		log.Fatalf("failed to load config: %v", err)
	}

	if config.BotToken == "" {
		log.Fatal("bot_token is required in config")
	}

	api := slack.New(config.BotToken)

	// Verify token
	authResp, err := api.AuthTest()
	if err != nil {
		log.Fatalf("slack auth failed: %v", err)
	}
	log.Printf("authenticated as %s in team %s", authResp.User, authResp.Team)

	// Start Unix socket listener
	socketPath := config.SocketPath
	if socketPath == "" {
		home, _ := os.UserHomeDir()
		socketPath = filepath.Join(home, ".revenant", "slack.sock")
	}

	// Clean up stale socket
	os.Remove(socketPath)

	// Ensure directory exists
	os.MkdirAll(filepath.Dir(socketPath), 0755)

	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		log.Fatalf("failed to listen on %s: %v", socketPath, err)
	}
	defer listener.Close()
	defer os.Remove(socketPath)

	log.Printf("listening on %s", socketPath)

	// Handle graceful shutdown
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		<-sigCh
		log.Println("shutting down")
		listener.Close()
		os.Remove(socketPath)
		os.Exit(0)
	}()

	// Accept connections
	for {
		conn, err := listener.Accept()
		if err != nil {
			if strings.Contains(err.Error(), "use of closed") {
				return
			}
			log.Printf("accept error: %v", err)
			continue
		}

		go handleConnection(conn, api, config)
	}
}

// ─── Connection Handling ───────────────────────────────────────────

func handleConnection(conn net.Conn, api *slack.Client, config *Config) {
	defer conn.Close()

	scanner := bufio.NewScanner(conn)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		var msg DaemonMessage
		if err := json.Unmarshal([]byte(line), &msg); err != nil {
			log.Printf("failed to parse message: %v", err)
			continue
		}

		switch msg.Type {
		case "inject":
			if msg.Card != nil {
				if err := postGhost(api, config, msg.Card); err != nil {
					log.Printf("failed to post ghost: %v", err)
				}
			}
		case "clear":
			// Ephemeral messages auto-clear in Slack - nothing to do
			log.Println("clear received (ephemeral messages auto-expire)")
		}
	}
}

// ─── Ghost Posting ─────────────────────────────────────────────────

func postGhost(api *slack.Client, config *Config, card *ContextCard) error {
	// Build the message with Slack Block Kit
	headerText := fmt.Sprintf(":ghost: *REVENANT* - %s", card.ProjectName)
	summaryText := card.Summary

	blocks := []slack.Block{
		slack.NewHeaderBlock(
			slack.NewTextBlockObject("plain_text", headerText, true, false),
		),
		slack.NewSectionBlock(
			slack.NewTextBlockObject("mrkdwn", summaryText, false, false),
			nil, nil,
		),
	}

	if card.NextStep != "" {
		blocks = append(blocks,
			slack.NewSectionBlock(
				slack.NewTextBlockObject("mrkdwn",
					fmt.Sprintf(":arrow_right: *Next:* %s", card.NextStep),
					false, false,
				),
				nil, nil,
			),
		)
	}

	blocks = append(blocks,
		slack.NewContextBlock("",
			slack.NewTextBlockObject("mrkdwn",
				fmt.Sprintf("_Ghost expires in %d min of activity_", card.TTLSeconds/60),
				false, false,
			),
		),
	)

	// Post as ephemeral message - visible only to the user
	_, err := api.PostEphemeral(
		config.ChannelID,
		config.UserID,
		slack.MsgOptionBlocks(blocks...),
		slack.MsgOptionText(card.Summary, false),
	)

	if err != nil {
		return fmt.Errorf("PostEphemeral failed: %w", err)
	}

	log.Printf("ghost posted to #%s for %s: %s",
		config.ChannelID, card.ProjectName, truncate(card.Summary, 60))

	return nil
}

// ─── Config Loading ────────────────────────────────────────────────

func loadConfig() (*Config, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}

	configPath := filepath.Join(home, ".config", "revenant", "slack.json")

	// Try JSON config first
	if data, err := os.ReadFile(configPath); err == nil {
		var config Config
		if err := json.Unmarshal(data, &config); err == nil {
			return &config, nil
		}
	}

	// Fall back to environment variables
	config := &Config{
		BotToken:  os.Getenv("REVENANT_SLACK_BOT_TOKEN"),
		UserID:    os.Getenv("REVENANT_SLACK_USER_ID"),
		ChannelID: os.Getenv("REVENANT_SLACK_CHANNEL_ID"),
	}

	if config.BotToken == "" {
		return nil, fmt.Errorf("no Slack config found at %s and REVENANT_SLACK_BOT_TOKEN not set", configPath)
	}

	return config, nil
}

// ─── Utilities ─────────────────────────────────────────────────────

func truncate(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen] + "..."
}

// Keep the compiler happy about unused import
var _ = time.Now
