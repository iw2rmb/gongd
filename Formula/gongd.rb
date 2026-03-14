class Gongd < Formula
  desc "Git-aware local filesystem event daemon over Unix sockets"
  homepage "https://github.com/iw2rmb/gongd"
  url "https://github.com/iw2rmb/gongd.git",
      tag: "0.1.0"
  license "MIT"
  head "https://github.com/iw2rmb/gongd.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/gongd --version").strip
  end
end
