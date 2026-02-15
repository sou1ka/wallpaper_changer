/**
 * simpletab.js
 * @author sou1ka
 */
window.addEventListener('load', function() {
	var tabanc = document.querySelectorAll('[data-tab-list]');
	var tabtgt = document.querySelectorAll('[data-tab-pane]');
	var overlay = null;

	if(tabtgt && tabtgt.length) {
		for(var i = 0, size = tabtgt.length; i < size; i++) {
			if(tabtgt[i].className.search('active') === -1) {
				tabtgt[i].style.display = 'none';
			}
		}
	}

	if(tabanc && tabanc.length) {
		for(var i = 0, size = tabanc.length; i < size; i++) {
			tabanc[i].addEventListener('click', function(e) {
				for(var x = 0, size = tabanc.length; x < size; x++) {
					tabanc[x].classList.remove('active');
				}
				e.target.classList.add('active');
				for(var x = 0, size = tabtgt.length; x < size; x++) {
					if(tabtgt[x].dataset.tabPane == e.target.dataset.tabList) {
						tabtgt[x].style.display = 'block';
					} else {
						tabtgt[x].style.display = 'none';
					}
				}
				if(!overlay) {
				  overlay = document.querySelector('.app-navbar-overlay');
				}
				if(overlay) {
				  overlay.click();
				}

			});
		}
	}


});
