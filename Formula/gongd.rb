class Gongd < Formula
  desc "Git-aware local filesystem event daemon over Unix sockets"
  homepage "https://github.com/iw2rmb/gongd"
  url "https://github.com/iw2rmb/gongd.git",
      tag: "v0.1.0"
  license "MIT"
  head "https://github.com/iw2rmb/gongd.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
    pkgshare.install "deploy/gongd.service", "deploy/local.gongd.plist"
  end

  service do
    run [opt_bin/"gongd"]
    keep_alive true
    environment_variables PATH: std_service_path_env
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/gongd --version").strip
  end
end
